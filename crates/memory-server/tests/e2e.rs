//! End-to-end: real HTTP gateway + SQLite (or Postgres when
//! `AGENT_MEMORY_TEST_POSTGRES_URL` is set), with a scripted
//! OpenAI-compatible LLM, driving L0 → L1 → L2 → L3 and skill extraction.

use std::time::Duration;

use axum::routing::post;
use axum::{Json, Router};
use memory_server::config::Config;
use memory_server::wiring;
use serde_json::{json, Value};

const L1_REPLY: &str = r###"[{"scene_name":"I (the AI) am planning a release with the user","memories":[
  {"content":"The user plans to release v2 next Monday","type":"episodic","priority":85,"source_message_ids":[]},
  {"content":"The user always runs integration tests before a release","type":"persona","priority":80,"source_message_ids":[]},
  {"content":"low value chatter","type":"persona","priority":10,"source_message_ids":[]}]}]"###;
const L2_REPLY: &str = r###"{"operations":[{"action":"create","file":"Release Planning","summary":"How the user ships releases","content":"## Core narrative\nRuns integration tests, then releases on Monday."}],"persona_update_request":null}"###;
const L3_REPLY: &str =
    "```markdown\n# User Narrative Profile\n> **Archetype**: careful shipper\n```";
const SKILL_REPLY: &str = r###"{"actions":[{"action":"create","name":"release-checklist","content":"---\nname: release-checklist\ndescription: Checklist before shipping a release\n---\n1. Run integration tests\n2. Tag and release"}],"summary":"saved release checklist"}"###;

/// Picks the canned reply by the system prompt's opening words.
async fn fake_llm(Json(req): Json<Value>) -> Json<Value> {
    let system = req["messages"][0]["content"].as_str().unwrap_or_default();
    let reply = if system.starts_with("You are a scene-segmentation") {
        L1_REPLY
    } else if system.starts_with("You reconcile") {
        "[]"
    } else if system.starts_with("You are a Memory Consolidation") {
        L2_REPLY
    } else if system.starts_with("You are a Persona Architect") {
        L3_REPLY
    } else if system.starts_with("You review a finished agent conversation") {
        SKILL_REPLY
    } else {
        "{}"
    };
    Json(json!({"choices": [{"message": {"role": "assistant", "content": reply}}]}))
}

async fn spawn_fake_llm() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(fake_llm)),
        )
        .await
        .unwrap()
    });
    format!("http://{addr}/v1")
}

struct Client {
    http: reqwest::Client,
    base: String,
    /// `x-memory-service-id` of every call.
    instance: String,
    /// Set by `bootstrap`: the admin, plus a plain member whose key `call`
    /// sends and whose team/agent/user `scoped` fills in.
    admin_key: String,
    member_key: String,
    team_id: String,
    agent_id: String,
    user_id: String,
    /// In-process access, for seeding data the HTTP API no longer accepts.
    state: memory_gateway::SharedState,
}

impl Client {
    async fn call(&self, path: &str, body: Value) -> Value {
        self.call_as(path, &self.member_key, body).await
    }

    fn scoped(&self, mut body: Value) -> Value {
        body["team_id"] = json!(self.team_id);
        body["agent_id"] = json!(self.agent_id);
        body["user_id"] = json!(self.user_id);
        body
    }

    /// Creates the admin (with its default team and agent) and a member of
    /// that team, then makes the member the default caller.
    async fn bootstrap(&mut self) {
        let admin = self
            .call_as(
                "/v3/internal/meta/user/init-admin",
                "",
                json!({"username": "admin"}),
            )
            .await["data"]
            .clone();
        self.admin_key = admin["user_key"].as_str().unwrap().to_owned();
        self.team_id = admin["team_id"].as_str().unwrap().to_owned();
        self.agent_id = admin["agent_id"].as_str().unwrap().to_owned();
        let member = self.create_member("user-e2e").await;
        self.user_id = member.0;
        self.member_key = member.1;
    }

    /// Creates a user, adds it to the bootstrap team, returns (id, key).
    async fn create_member(&self, username: &str) -> (String, String) {
        let user = self
            .call_as(
                "/v3/meta/user/create",
                &self.admin_key,
                json!({"username": username}),
            )
            .await["data"]
            .clone();
        let user_id = user["user_id"].as_str().unwrap().to_owned();
        let added = self
            .call_as(
                "/v3/meta/team-member/add",
                &self.admin_key,
                json!({"team_id": self.team_id, "user_id": user_id}),
            )
            .await;
        assert_eq!(added["code"], 0, "{added}");
        (
            user_id,
            user["default_user_key"].as_str().unwrap().to_owned(),
        )
    }

    /// Polls `stats/overview` until `pred` holds.
    async fn wait_for(&self, pred: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..100 {
            let stats = self.call("/v3/stats/overview", json!({})).await["data"].clone();
            if pred(&stats) {
                return stats;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("pipeline did not reach the expected state");
    }
}

async fn start() -> (tempfile::TempDir, Client) {
    let base_url = spawn_fake_llm().await;
    start_with(|config| {
        config.llm.base_url = base_url;
        config.llm.api_key = "fake".into();
    })
    .await
}

async fn start_with(configure: impl FnOnce(&mut Config)) -> (tempfile::TempDir, Client) {
    let data = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config.data.base_dir = data.path().to_string_lossy().into_owned();
    config.server.api_key = Some("test-key".into());
    configure(&mut config);
    config.memory.pipeline.l2_delay_after_l1_seconds = 0;
    config.memory.pipeline.l2_min_interval_seconds = 0;
    config.worker.scan_interval_ms = 50;
    let app = wiring::build(&config).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(memory_gateway::serve(
        listener,
        app.state.clone(),
        std::future::pending(),
    ));
    // Unique per run: on Postgres instances outlive the test process.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut client = Client {
        http: reqwest::Client::new(),
        base,
        instance: format!("ins-e2e-{}-{nanos}", std::process::id()),
        admin_key: String::new(),
        member_key: String::new(),
        team_id: String::new(),
        agent_id: String::new(),
        user_id: String::new(),
        state: app.state.clone(),
    };
    client.bootstrap().await;
    (data, client)
}

#[tokio::test(flavor = "multi_thread")]
async fn conversation_is_distilled_into_atoms_scenes_and_persona() {
    let (_data, client) = start().await;
    assert_conversation_is_distilled(&client).await;
}

/// The same flow with Redis coordinating state, queue and locks. Skipped
/// unless `AGENT_MEMORY_TEST_REDIS_URL` is set.
#[tokio::test(flavor = "multi_thread")]
async fn redis_coordination_runs_the_pipeline() {
    let Ok(redis_url) = std::env::var("AGENT_MEMORY_TEST_REDIS_URL") else {
        eprintln!("skipping: set AGENT_MEMORY_TEST_REDIS_URL to run against Redis");
        return;
    };
    let base_url = spawn_fake_llm().await;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let (_data, client) = start_with(|config| {
        config.llm.base_url = base_url;
        config.llm.api_key = "fake".into();
        config.coordination.backend = "redis".into();
        config.coordination.redis.url = redis_url;
        // A fresh namespace, so earlier runs' state never interferes.
        config.coordination.redis.key_prefix = format!("amem-e2e-{nanos}");
    })
    .await;
    assert_conversation_is_distilled(&client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pipeline_runs_on_the_postgres_backend() {
    let Ok(url) = std::env::var("AGENT_MEMORY_TEST_POSTGRES_URL") else {
        eprintln!("skipping: set AGENT_MEMORY_TEST_POSTGRES_URL to run against Postgres");
        return;
    };
    let base_url = spawn_fake_llm().await;
    let (_data, client) = start_with(|config| {
        config.llm.base_url = base_url;
        config.llm.api_key = "fake".into();
        config.storage.backend = "postgres".into();
        config.storage.postgres.url = url;
    })
    .await;

    let health: Value = client
        .http
        .get(format!("{}/health", client.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["stores"]["backend"], "postgres", "{health}");
    assert_eq!(health["stores"]["ftsAvailable"], true);

    assert_conversation_is_distilled(&client).await;
    assert_tool_calls_become_a_skill(&client).await;

    let destroyed = client
        .call(
            "/v3/instance/destroy",
            json!({"instance_id": client.instance}),
        )
        .await;
    assert_eq!(
        destroyed["data"]["cleaned"]["storage"]["postgres"]["removed"], true,
        "{destroyed}"
    );
    let stats = client.call("/v3/stats/overview", json!({})).await;
    assert_eq!(stats["data"]["l1"]["total"], 0, "{stats}");
    // Reading re-created the (empty) schema lazily; leave nothing behind.
    client
        .call(
            "/v3/instance/destroy",
            json!({"instance_id": client.instance}),
        )
        .await;
}

async fn assert_conversation_is_distilled(client: &Client) {
    let added = client
        .call(
            "/v3/conversation/add",
            client.scoped(json!({"session_id": "s1", "messages": [
                {"role": "user", "content": "We release v2 next Monday; I always run integration tests first."},
                {"role": "assistant", "content": "Noted, I will remind you."}
            ]})),
        )
        .await;
    assert_eq!(added["code"], 0, "{added}");

    // Warm-up threshold 1 → L1 immediately → L2 (no delay) → L3 cold start.
    let stats = client
        .wait_for(|s| {
            // Each layer logs its run after writing its output.
            s["l1"]["total"] == 2
                && s["l2"] == 1
                && s["l3"] == 1
                && s["generation_logs"].as_u64().unwrap_or(0) >= 3
        })
        .await;
    assert_eq!(
        stats["l1"]["episodic"], 1,
        "low-priority memory must be dropped: {stats}"
    );

    let scenes = client
        .call("/v3/scenario/ls", client.scoped(json!({})))
        .await;
    assert_eq!(scenes["data"]["entries"][0]["path"], "Release-Planning.md");
    assert_eq!(scenes["data"]["entries"][0]["heat"], 1);

    let persona = client.call("/v3/core/read", client.scoped(json!({}))).await;
    let content = persona["data"]["content"].as_str().unwrap();
    assert!(content.starts_with("# User Narrative Profile"), "{content}");
    assert!(content.contains("Scene Navigation"));

    let recall = client
        .call(
            "/v3/recall",
            client.scoped(json!({"query": "when is the v2 release?"})),
        )
        .await;
    let memories = recall["data"]["memories"].as_array().unwrap();
    assert!(
        memories
            .iter()
            .any(|m| m["content"].as_str().unwrap().contains("next Monday")),
        "{recall}"
    );
    assert!(recall["data"]["append_system_context"]
        .as_str()
        .unwrap()
        .contains("<user-persona>"));

    // Isolation: a member cannot even ask for another user's L1, and that
    // user (seen through the admin) shares L2/L3 but not L1.
    let peek = client
        .call(
            "/v3/atomic/query",
            json!({"team_id": client.team_id, "agent_id": client.agent_id, "user_id": "someone-else"}),
        )
        .await;
    assert_eq!(peek["code"], 403, "{peek}");
    let other = client
        .call_as(
            "/v3/atomic/query",
            &client.admin_key,
            json!({"team_id": client.team_id, "agent_id": client.agent_id, "user_id": "someone-else"}),
        )
        .await;
    assert_eq!(other["data"]["total"], 0, "{other}");
}

#[tokio::test(flavor = "multi_thread")]
async fn tool_heavy_conversations_are_archived_into_skills() {
    let (_data, client) = start().await;
    assert_tool_calls_become_a_skill(&client).await;
}

async fn assert_tool_calls_become_a_skill(client: &Client) {
    let tool_calls: Vec<Value> = (0..10)
        .flat_map(|i| {
            [
                json!({"role": "tool_call", "content": format!("run step {i}"), "tool_name": "shell", "tool_call_id": format!("c{i}")}),
                json!({"role": "tool_result", "content": "ok", "tool_name": "shell", "tool_call_id": format!("c{i}")}),
            ]
        })
        .collect();
    let result = client
        .call(
            "/v3/skill/conversation/add",
            client.scoped(json!({"session_id": "s-skill", "messages": tool_calls})),
        )
        .await;
    assert_eq!(result["data"]["status"], "archived", "{result}");
    assert_eq!(result["data"]["archived"]["reason"], "tool_calls");

    client.wait_for(|s| s["skills"] == 1).await;
    let listing = client
        .call("/v3/skill/listing", client.scoped(json!({})))
        .await;
    assert!(
        listing["data"]["listing"]
            .as_str()
            .unwrap()
            .contains("release-checklist"),
        "{listing}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_without_bearer_token_are_rejected() {
    let (_data, client) = start().await;
    let res = client
        .http
        .post(format!("{}/v3/stats/overview", client.base))
        .header("x-memory-service-id", &client.instance)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let health = client
        .http
        .get(format!("{}/health", client.base))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
}

impl Client {
    /// Sends the Bearer key and, unless empty, `user_key`.
    async fn call_as(&self, path: &str, user_key: &str, body: Value) -> Value {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth("test-key")
            .header("x-memory-service-id", &self.instance);
        if !user_key.is_empty() {
            req = req.header("x-memory-user-key", user_key);
        }
        req.json(&body).send().await.unwrap().json().await.unwrap()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn other_users_cannot_read_or_change_foreign_data() {
    let (_data, client) = start().await;
    let (admin_key, team, agent) = (&client.admin_key, &client.team_id, &client.agent_id);
    let outsider = client
        .call_as(
            "/v3/meta/user/create",
            admin_key,
            json!({"username": "mallory"}),
        )
        .await["data"]
        .clone();
    let mallory_key = outsider["default_user_key"].as_str().unwrap();
    let admin_id = client
        .call_as("/v3/meta/auth/verify", "", json!({"user_key": admin_key}))
        .await["data"]["user"]["user_id"]
        .clone();

    let team_read = client
        .call_as("/v3/meta/team/get", mallory_key, json!({"team_id": team}))
        .await;
    assert_eq!(
        team_read["code"], 403,
        "non-members must not read the team: {team_read}"
    );
    let spy = client
        .call_as(
            "/v3/meta/asset/list-accessible",
            mallory_key,
            json!({"user_id": admin_id}),
        )
        .await;
    assert_eq!(
        spy["code"], 403,
        "listing another user's assets must be refused: {spy}"
    );
    let loadout = client
        .call_as(
            "/v3/meta/agent-fixed-asset/list-with-detail",
            mallory_key,
            json!({"agent_id": agent}),
        )
        .await;
    assert_eq!(loadout["code"], 403, "{loadout}");

    let skill = client
        .call_as(
            "/v3/skill/create",
            admin_key,
            json!({"team_id": team, "agent_id": agent, "name": "secret-runbook",
                   "content": "---\nname: secret-runbook\ndescription: internal\n---\nsteps"}),
        )
        .await;
    let skill_id = skill["data"]["skill_id"].as_str().unwrap();
    let foreign_read = client
        .call_as(
            "/v3/skill/get",
            mallory_key,
            json!({"skill_id": skill_id, "team_id": team, "agent_id": agent}),
        )
        .await;
    assert_eq!(foreign_read["code"], 403, "{foreign_read}");
    let memory_read = client
        .call_as(
            "/v3/recall",
            mallory_key,
            json!({"team_id": team, "agent_id": agent, "query": "x"}),
        )
        .await;
    assert_eq!(memory_read["code"], 403, "{memory_read}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_data_plane_is_authorized_by_user_key_alone() {
    let (_data, client) = start().await;
    let post = |path: &str| {
        client
            .http
            .post(format!("{}{path}", client.base))
            .header("x-memory-service-id", &client.instance)
    };
    let anonymous = post("/v3/recall")
        .json(&client.scoped(json!({"query": "x"})))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401, "a user key is required");

    // No Bearer key: remote clients only hold their own user key.
    let recall: Value = post("/v3/recall")
        .header("x-memory-user-key", &client.member_key)
        .json(&json!({"team_id": client.team_id, "agent_id": client.agent_id, "query": "x"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        recall["code"], 0,
        "user_id defaults to the caller: {recall}"
    );

    let no_team = client
        .call(
            "/v3/recall",
            json!({"agent_id": client.agent_id, "query": "x"}),
        )
        .await;
    assert_eq!(
        no_team["code"], 400,
        "there is no default team bucket: {no_team}"
    );
    let unknown_agent = client
        .call(
            "/v3/recall",
            json!({"team_id": client.team_id, "agent_id": "nope", "query": "x"}),
        )
        .await;
    assert_eq!(unknown_agent["code"], 403, "{unknown_agent}");
    for gone in ["/recall", "/capture"] {
        assert_eq!(post(gone).send().await.unwrap().status(), 404, "{gone}");
    }
}

/// A stand-in for a local LLM CLI (`claude -p`, `ollama run` …): reads the
/// prompt from stdin, records its working directory, prints a canned reply.
#[cfg(unix)]
fn write_fake_cli(dir: &std::path::Path) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let reply = |text: &str| text.replace('\'', "'\\''");
    let script = format!(
        "#!/bin/sh\npwd > ran-in.txt\nprompt=$(cat)\ncase \"$prompt\" in\n\
         *'scene-segmentation'*) printf '%s' '{}' ;;\n\
         *'You reconcile'*) printf '[]' ;;\n\
         *'Memory Consolidation'*) printf '%s' '{}' ;;\n\
         *'Persona Architect'*) printf '%s' '{}' ;;\n\
         *) printf '{{}}' ;;\nesac\n",
        reply(L1_REPLY),
        reply(L2_REPLY),
        reply(L3_REPLY)
    );
    let path = dir.join("fake-llm.sh");
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_local_cli_can_serve_as_the_llm() {
    let project = tempfile::tempdir().unwrap();
    let cli = write_fake_cli(project.path());
    let workdir = project.path().to_string_lossy().into_owned();
    let (_data, client) = start_with(|config| {
        config.llm.provider = "cli".into();
        config.llm.cli.command = cli.to_string_lossy().into_owned();
        config.llm.cli.working_dir = Some(workdir);
    })
    .await;
    client
        .call(
            "/v3/conversation/add",
            client.scoped(json!({"session_id": "s-cli", "messages": [{"role": "user", "content": "We release v2 next Monday."}]})),
        )
        .await;
    client
        .wait_for(|s| s["l1"]["total"] == 2 && s["l3"] == 1)
        .await;
    let ran_in = std::fs::read_to_string(project.path().join("ran-in.txt")).unwrap();
    assert_eq!(
        std::fs::canonicalize(ran_in.trim()).unwrap(),
        std::fs::canonicalize(project.path()).unwrap(),
        "the CLI must run in the configured project directory"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_system_admin_manages_teams_it_does_not_belong_to() {
    let (_data, client) = start().await;
    let admin = &client.admin_key;
    let (alice, alice_key) = client.create_member("alice").await;
    let team = client
        .call_as(
            "/v3/meta/team/create",
            admin,
            json!({"name": "research", "owner_user_id": alice}),
        )
        .await["data"]["team_id"]
        .clone();
    let agent = client
        .call_as(
            "/v3/meta/agent/create",
            admin,
            json!({"team_id": team, "owner_user_id": alice, "name": "claude-code"}),
        )
        .await;
    assert_eq!(agent["code"], 0, "admins need no membership: {agent}");

    let all = client
        .call_as("/v3/meta/team/list", admin, json!({"all": true}))
        .await;
    assert_eq!(all["data"]["total"], 2, "bootstrap team + research: {all}");
    let refused = client
        .call_as("/v3/meta/team/list", &alice_key, json!({"all": true}))
        .await;
    assert_eq!(refused["code"], 403, "{refused}");

    let recall = client
        .call_as(
            "/v3/recall",
            &alice_key,
            json!({"team_id": team, "agent_id": agent["data"]["agent_id"], "query": "x"}),
        )
        .await;
    assert_eq!(
        recall["code"], 0,
        "the owner is a member of the new team: {recall}"
    );
}

/// The `amem` client against a live server: redaction, both capture
/// requests and the offline queue.
#[tokio::test(flavor = "multi_thread")]
async fn the_amem_client_redacts_queues_and_delivers() {
    use memory_client::{
        api::Api, capture::capture, config::Config, queue::Queue, transcript::Msg,
    };

    let (_data, client) = start().await;
    let queue_dir = tempfile::tempdir().unwrap();
    let cfg = Config {
        server_url: client.base.clone(),
        service_id: client.instance.clone(),
        user_key: client.member_key.clone(),
        team_id: client.team_id.clone(),
        agent_id: client.agent_id.clone(),
    };
    let msg = |role: &'static str, content: &str| Msg {
        role,
        content: content.into(),
        tool_name: Some("shell".into()),
        tool_call_id: Some("c1".into()),
    };
    let turn = vec![
        Msg {
            tool_name: None,
            tool_call_id: None,
            ..msg(
                "user",
                "We release v2 next Monday. My key is sk-ant-api03-abcdefghijklmnopqrstuvwxyz",
            )
        },
        msg("tool_call", r#"{"command":"ls"}"#),
        msg("tool_result", "README.md"),
        Msg {
            tool_name: None,
            tool_call_id: None,
            ..msg("assistant", "Noted.")
        },
    ];

    let (queue_path, live, dead) = (
        queue_dir.path().to_owned(),
        cfg.clone(),
        Config {
            server_url: "http://127.0.0.1:9".into(),
            ..cfg
        },
    );
    let (offline, flushed, online) = tokio::task::spawn_blocking(move || {
        let queue = Queue::new(&queue_path);
        let down = Api::new(dead, Duration::from_secs(2)).unwrap();
        let offline = capture(&down, &queue, "s-amem", turn.clone());
        let up = Api::new(live, Duration::from_secs(10)).unwrap();
        let flushed = queue.flush(&up);
        let online = capture(&up, &queue, "s-amem-2", turn);
        (offline, flushed, online)
    })
    .await
    .unwrap();
    assert_eq!(
        (offline.sent, offline.queued),
        (0, 2),
        "server down → both requests queued"
    );
    assert_eq!((flushed.sent, flushed.remaining), (2, 0), "{flushed:?}");
    assert_eq!((online.sent, online.queued), (2, 0), "{:?}", online.error);

    let stored = client
        .call(
            "/v3/conversation/query",
            client.scoped(json!({"limit": 10})),
        )
        .await;
    let contents: Vec<&str> = stored["data"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"].as_str().unwrap())
        .collect();
    assert_eq!(contents.len(), 4, "two turns × user + assistant: {stored}");
    assert!(
        contents.iter().all(|c| !c.contains("sk-ant-")),
        "secrets are redacted: {contents:?}"
    );
    assert!(contents
        .iter()
        .any(|c| c.contains("[REDACTED:anthropic-key]")));
}

/// Upgrade path: data written to the legacy `default` buckets (before
/// team/agent were required) is moved to a real scope, pipeline state
/// included, and becomes reachable again.
#[tokio::test(flavor = "multi_thread")]
async fn legacy_default_data_migrates_to_a_real_scope() {
    let (_data, client) = start().await;
    assert_legacy_migration(&client).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn legacy_default_data_migrates_on_postgres() {
    let Ok(url) = std::env::var("AGENT_MEMORY_TEST_POSTGRES_URL") else {
        eprintln!("skipping: set AGENT_MEMORY_TEST_POSTGRES_URL to run against Postgres");
        return;
    };
    let base_url = spawn_fake_llm().await;
    let (_data, client) = start_with(|config| {
        config.llm.base_url = base_url;
        config.llm.api_key = "fake".into();
        config.storage.backend = "postgres".into();
        config.storage.postgres.url = url;
    })
    .await;
    assert_legacy_migration(&client).await;
    client
        .call_as(
            "/v3/instance/destroy",
            "",
            json!({"instance_id": client.instance}),
        )
        .await;
}

/// Writes a turn to the legacy `default/default/default` bucket, as a
/// pre-0.1.1 client did (the data plane no longer accepts it over HTTP).
async fn seed_legacy(client: &Client, session: &str, text: &str) {
    seed_legacy_as(client, None, None, session, text).await;
}

/// Same, for a pre-0.1.1 client that named its agent and/or user but left
/// the team empty, so the rows sit outside `default/default/default`.
async fn seed_legacy_as(
    client: &Client,
    agent: Option<&str>,
    user: Option<&str>,
    session: &str,
    text: &str,
) {
    use memory_application::memory::service::NewMessage;
    use memory_domain::memory::Role;
    use memory_domain::scope::{MemoryScope, SegmentId};

    let instance = SegmentId::parse("instance", &client.instance).unwrap();
    let legacy = MemoryScope::from_optional(None, agent, user, None).unwrap();
    let message = |role, content: &str| NewMessage {
        role,
        content: content.into(),
        timestamp: None,
        recorded_at: None,
    };
    client
        .state
        .services
        .memory
        .add_conversation(
            &instance,
            &legacy,
            session,
            vec![
                message(Role::User, text),
                message(Role::Assistant, "Noted."),
            ],
        )
        .await
        .unwrap();
}

async fn assert_legacy_migration(client: &Client) {
    seed_legacy(
        client,
        "legacy",
        "We release v2 next Monday; I always run integration tests first.",
    )
    .await;
    client
        .wait_for(|s| s["l1"]["total"] == 2 && s["l2"] == 1 && s["l3"] == 1)
        .await;

    let agent = client
        .call_as(
            "/v3/meta/agent/create",
            &client.admin_key,
            json!({"team_id": client.team_id, "owner_user_id": client.user_id, "name": "hermes"}),
        )
        .await["data"]["agent_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let target = json!({"team_id": client.team_id, "agent_id": agent, "user_id": client.user_id});
    let migrate = |dry_run: bool| {
        let mut body = target.clone();
        body["dry_run"] = json!(dry_run);
        client.call_as("/v3/internal/migrate/default-scope", "", body)
    };

    let dry = migrate(true).await;
    assert_eq!(dry["data"]["moved"]["conversations"], 2, "{dry}");
    assert_eq!(dry["data"]["moved"]["atoms"], 2);
    assert!(
        dry["data"]["moved"]["profile_files"].as_u64().unwrap() >= 2,
        "scene + persona: {dry}"
    );
    let scoped = |body: Value| {
        let mut b = body;
        b["team_id"] = json!(client.team_id);
        b["agent_id"] = json!(agent);
        b
    };
    let before = client.call("/v3/atomic/query", scoped(json!({}))).await;
    assert_eq!(
        before["data"]["total"], 0,
        "a dry run changes nothing: {before}"
    );

    let done = migrate(false).await;
    assert_eq!(done["data"]["moved"], dry["data"]["moved"], "{done}");
    assert_eq!(done["data"]["session_states"], 1);
    assert_eq!(done["data"]["profile_states"], 1);

    let recall = client
        .call(
            "/v3/recall",
            scoped(json!({"query": "when is the v2 release?"})),
        )
        .await;
    assert!(
        recall["data"]["memories"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["content"].as_str().unwrap().contains("next Monday")),
        "{recall}"
    );
    let persona = client.call("/v3/core/read", scoped(json!({}))).await;
    assert!(
        persona["data"]["content"]
            .as_str()
            .unwrap()
            .contains("User Narrative Profile"),
        "{persona}"
    );

    let again = migrate(false).await;
    assert_eq!(again["code"], 0, "nothing left to move is a no-op: {again}");
    assert_eq!(again["data"]["moved"]["conversations"], 0);
    seed_legacy(client, "late", "One more legacy turn.").await;
    let conflict = migrate(false).await;
    assert_eq!(
        conflict["code"], 409,
        "the target now has L2/L3: {conflict}"
    );
    let stranger = client
        .call_as(
            "/v3/internal/migrate/default-scope",
            "",
            json!({"team_id": client.team_id, "agent_id": client.agent_id, "user_id": "usr-nobody"}),
        )
        .await;
    assert_eq!(stranger["code"], 400, "{stranger}");
}

/// `from_*` select a named legacy bucket: rows written as
/// `default/<agent>/<user>` move, and without them nothing does.
#[tokio::test(flavor = "multi_thread")]
async fn named_legacy_scope_migrates_with_from_fields() {
    let (_data, client) = start().await;
    seed_legacy_as(
        &client,
        Some("hermes"),
        Some("alice"),
        "named",
        "We release v2 next Monday.",
    )
    .await;
    client
        .wait_for(|s| s["l1"]["total"] == 2 && s["l2"] == 1 && s["l3"] == 1)
        .await;

    let agent = client
        .call_as(
            "/v3/meta/agent/create",
            &client.admin_key,
            json!({"team_id": client.team_id, "owner_user_id": client.user_id, "name": "hermes"}),
        )
        .await["data"]["agent_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let migrate = |from: Value, dry_run: bool| {
        let mut body = json!({"team_id": client.team_id, "agent_id": agent, "user_id": client.user_id, "dry_run": dry_run});
        body.as_object_mut()
            .unwrap()
            .extend(from.as_object().unwrap().clone());
        client.call_as("/v3/internal/migrate/default-scope", "", body)
    };

    let default_only = migrate(json!({}), true).await;
    assert_eq!(
        default_only["data"]["moved"]["conversations"], 0,
        "{default_only}"
    );
    let from = json!({"from_agent_id": "hermes", "from_user_id": "alice"});
    let dry = migrate(from.clone(), true).await;
    assert_eq!(dry["data"]["moved"]["conversations"], 2, "{dry}");
    assert_eq!(dry["data"]["moved"]["atoms"], 2);

    let done = migrate(from, false).await;
    assert_eq!(done["data"]["moved"], dry["data"]["moved"], "{done}");
    assert_eq!(done["data"]["session_states"], 1);
    assert_eq!(done["data"]["profile_states"], 1);
    let moved = client
        .call(
            "/v3/atomic/query",
            json!({"team_id": client.team_id, "agent_id": agent}),
        )
        .await;
    assert_eq!(moved["data"]["total"], 2, "{moved}");
}

/// `internal/upgrade` needs only the instance key: it finds the admin,
/// reuses or creates team and agent by name, moves the legacy data and
/// issues a key that reaches it. Re-running it changes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn upgrade_rehomes_legacy_data_with_the_instance_key_only() {
    let (_data, client) = start().await;
    seed_legacy(&client, "legacy", "Our staging database is called KESTREL.").await;
    client
        .wait_for(|s| s["l1"]["total"] == 2 && s["l2"] == 1 && s["l3"] == 1)
        .await;
    let upgrade = |body: Value| client.call_as("/v3/internal/upgrade", "", body);

    let plan = upgrade(json!({"agent_name": "hermes", "dry_run": true})).await;
    assert_eq!(plan["code"], 0, "{plan}");
    assert_eq!(
        plan["data"]["team_id"],
        json!(client.team_id),
        "bootstrap team found by name: {plan}"
    );
    assert_eq!(
        plan["data"]["agent_id"],
        Value::Null,
        "agent not created by a dry run"
    );
    assert_eq!(plan["data"]["moved"]["conversations"], 2);

    let done = upgrade(json!({"agent_name": "hermes", "issue_key": true})).await;
    assert_eq!(done["code"], 0, "{done}");
    let data = &done["data"];
    assert_eq!(
        (data["team_created"].clone(), data["agent_created"].clone()),
        (json!(false), json!(true))
    );
    assert_eq!(data["moved"], plan["data"]["moved"]);
    assert_eq!(data["username"], "admin");
    let key = data["user_key"].as_str().expect("a fresh key").to_owned();
    let agent = data["agent_id"].as_str().unwrap().to_owned();

    let recall = client
        .call_as("/v3/recall", &key, json!({"team_id": client.team_id, "agent_id": agent, "query": "when is the v2 release?"}))
        .await;
    // The fake LLM distills fixed atoms; what matters is that they moved.
    assert!(
        recall["data"]["memories"]
            .to_string()
            .contains("next Monday"),
        "{recall}"
    );
    assert!(recall["data"]["append_system_context"]
        .as_str()
        .unwrap()
        .contains("User Narrative Profile"));

    let again = upgrade(json!({"agent_name": "hermes"})).await;
    assert_eq!(again["code"], 0, "{again}");
    assert_eq!(again["data"]["agent_id"], json!(agent), "same agent reused");
    assert_eq!(again["data"]["agent_created"], false);
    assert_eq!(again["data"]["moved"]["conversations"], 0);
    assert!(again["data"].get("user_key").is_none());

    let missing = upgrade(json!({"admin_username": "nobody"})).await;
    assert_eq!(missing["code"], 404, "{missing}");
}
