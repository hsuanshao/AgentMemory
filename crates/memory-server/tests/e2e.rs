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
/// L2's answer when a scene named `Legacy-Release.md` already exists.
const L2_MERGE_REPLY: &str = r###"{"operations":[{"action":"merge","file":"Release Planning","merge_from":["Legacy-Release.md"],"summary":"How the user ships releases","content":"## Core narrative\nRuns integration tests, then releases on Monday."}],"persona_update_request":null}"###;
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
        let input = req["messages"][1]["content"].as_str().unwrap_or_default();
        if input.contains("### Legacy-Release.md") {
            L2_MERGE_REPLY
        } else {
            L2_REPLY
        }
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
    /// `x-memory-department-id` of every call.
    department: String,
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

    /// `body` in the bootstrap team and agent, for `user` (default: the
    /// caller, which the server fills in).
    fn scoped_for(&self, mut body: Value, user: Option<&str>) -> Value {
        body["team_id"] = json!(self.team_id);
        body["agent_id"] = json!(self.agent_id);
        if let Some(user) = user {
            body["user_id"] = json!(user);
        }
        body
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
        self.create_member_as(username, "member").await
    }

    async fn create_member_as(&self, username: &str, role: &str) -> (String, String) {
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
                json!({"team_id": self.team_id, "user_id": user_id, "role": role}),
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
    // Unique per run: on Postgres departments outlive the test process.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let mut client = Client {
        http: reqwest::Client::new(),
        base,
        department: format!("ins-e2e-{}-{nanos}", std::process::id()),
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
            "/v3/department/destroy",
            json!({"department_id": client.department}),
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
            "/v3/department/destroy",
            json!({"department_id": client.department}),
        )
        .await;
}

/// Scenes are append-only: rewrites, merges and removals add versions, and
/// every earlier version stays readable.
#[tokio::test(flavor = "multi_thread")]
async fn scene_versions_survive_merges_and_removals() {
    let (_data, client) = start().await;
    let write = |content: &'static str| {
        client.call(
            "/v3/scenario/write",
            client.scoped(
                json!({"path": "Legacy-Release.md", "content": content, "summary": "old notes"}),
            ),
        )
    };
    assert_eq!(write("first draft").await["data"]["version"], 1);
    assert_eq!(write("second draft").await["data"]["version"], 2);
    let read = |path: &'static str, version: Option<u32>| {
        client.call(
            "/v3/scenario/read",
            client.scoped(json!({"path": path, "version": version})),
        )
    };
    let first = read("Legacy-Release.md", Some(1)).await;
    assert!(
        first["data"]["content"]
            .as_str()
            .unwrap()
            .ends_with("first draft"),
        "{first}"
    );

    // L2 merges the legacy scene into a new one.
    client
        .call(
            "/v3/conversation/add",
            client.scoped(json!({"session_id": "s1", "messages": [
                {"role": "user", "content": "We release v2 next Monday; I always run integration tests first."},
                {"role": "assistant", "content": "Noted, I will remind you."}
            ]})),
        )
        .await;
    client
        .wait_for(|s| s["l1"]["total"] == 2 && s["generation_logs"].as_u64().unwrap_or(0) >= 2)
        .await;
    let scenes = client
        .call("/v3/scenario/ls", client.scoped(json!({})))
        .await;
    let entries = scenes["data"]["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1, "{scenes}");
    assert_eq!(entries[0]["path"], "Release-Planning.md");
    assert_eq!(entries[0]["heat"], 2, "the merge adds the absorbed heat");

    let merged_away = read("Legacy-Release.md", None).await;
    assert!(merged_away["data"]["content"].is_null(), "{merged_away}");
    let kept = read("Legacy-Release.md", Some(2)).await;
    assert!(
        kept["data"]["content"]
            .as_str()
            .unwrap()
            .ends_with("second draft"),
        "{kept}"
    );

    // Removing retires the scene; its versions stay.
    let removed = client
        .call(
            "/v3/scenario/rm",
            client.scoped(json!({"path": "Release-Planning.md"})),
        )
        .await;
    assert_eq!(removed["data"]["removed"], 1, "{removed}");
    let scenes = client
        .call("/v3/scenario/ls", client.scoped(json!({})))
        .await;
    assert_eq!(scenes["data"]["total"], 0, "{scenes}");
    let old = read("Release-Planning.md", Some(1)).await;
    assert!(
        old["data"]["content"]
            .as_str()
            .unwrap()
            .contains("Core narrative"),
        "{old}"
    );
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
        .header("x-memory-department-id", &client.department)
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

#[tokio::test(flavor = "multi_thread")]
async fn clients_older_than_departments_are_told_to_update() {
    let (_data, client) = start().await;
    let res = client
        .http
        .post(format!("{}/v3/stats/overview", client.base))
        .bearer_auth("test-key")
        .header("x-memory-service-id", &client.department)
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 401);
    let body: Value = res.json().await.unwrap();
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("renamed in 0.3.0"),
        "{body}"
    );
}

impl Client {
    /// Sends the Bearer key and, unless empty, `user_key`.
    async fn call_as(&self, path: &str, user_key: &str, body: Value) -> Value {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base))
            .bearer_auth("test-key")
            .header("x-memory-department-id", &self.department);
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
            .header("x-memory-department-id", &client.department)
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
        department_id: client.department.clone(),
        user_key: client.member_key.clone(),
        team_id: client.team_id.clone(),
        agent_id: client.agent_id.clone(),
        skip_prompt_prefixes: String::new(),
    };
    let msg = |role: &'static str, content: &str| Msg {
        role,
        content: content.into(),
        tool_name: Some("shell".into()),
        tool_call_id: Some("c1".into()),
        timestamp: None,
        interjected: false,
    };
    let turn = vec![
        Msg {
            tool_name: None,
            tool_call_id: None,
            // Assembled at runtime so no token-shaped literal trips a
            // secret scanner.
            ..msg(
                "user",
                &format!(
                    "We release v2 next Monday. My key is sk-ant-{}",
                    "x".repeat(24)
                ),
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
            "/v3/department/destroy",
            "",
            json!({"department_id": client.department}),
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

    let department = SegmentId::parse("department", &client.department).unwrap();
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
            &department,
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

/// `internal/upgrade` needs only the server API key: it finds the admin,
/// reuses or creates team and agent by name, moves the legacy data and
/// issues a key that reaches it. Re-running it changes nothing.
#[tokio::test(flavor = "multi_thread")]
async fn upgrade_rehomes_legacy_data_with_the_server_key_only() {
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

async fn dead_letters(client: &Client) -> Vec<Value> {
    client
        .call("/v3/pipeline/dead-letter/list", json!({}))
        .await["data"]["items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// An LLM endpoint that is always overloaded.
async fn spawn_failing_llm() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/chat/completions",
                post(|| async {
                    (
                        axum::http::StatusCode::SERVICE_UNAVAILABLE,
                        "model is at capacity",
                    )
                }),
            ),
        )
        .await
        .unwrap()
    });
    format!("http://{addr}/v1")
}

#[tokio::test(flavor = "multi_thread")]
async fn failed_generations_are_logged_and_dead_letters_redriven() {
    let base_url = spawn_failing_llm().await;
    let (_data, client) = start_with(|config| {
        config.llm.base_url = base_url;
        config.llm.api_key = "fake".into();
        config.llm.model = "fake-model".into();
        // The first failure dead-letters the job: no backoff to wait for.
        config.worker.max_retries = 0;
    })
    .await;
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

    let mut letters = Vec::new();
    for _ in 0..100 {
        letters = dead_letters(&client).await;
        if !letters.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(letters.len(), 1, "the L1 job is dead-lettered");
    assert_eq!(letters[0]["kind"], "l1");
    assert_eq!(letters[0]["job"]["L1"]["session_id"], "s1");
    // Its input was dropped: the letter keeps the job that redoes exactly it.
    let range = letters[0]["job"]["L1"]["range"].clone();
    assert!(
        range["first"].as_i64().unwrap() <= range["last"].as_i64().unwrap(),
        "{range}"
    );
    let error = letters[0]["error"].as_str().unwrap();
    assert!(error.contains("503"), "{error}");
    assert!(letters[0]["dead_at_ms"].as_i64().unwrap() > 0);

    // The failure is in the generation logs, with the model and the error.
    let failed = client
        .call(
            "/v3/memory-generation-log/list",
            json!({"status": "dropped"}),
        )
        .await["data"]["items"]
        .clone();
    assert_eq!(failed[0]["layer"], "l1");
    assert_eq!(
        failed[0]["input_refs"].as_array().unwrap().len(),
        2,
        "{failed}"
    );
    assert_eq!(failed[0]["model"], "fake-model");
    assert_eq!(failed[0]["session_id"], "s1");
    assert!(
        failed[0]["error_message"]
            .as_str()
            .unwrap()
            .contains("model is at capacity"),
        "{failed}"
    );
    let summary = client
        .call("/v3/memory-generation-log/summary", json!({"hours": 1}))
        .await["data"]
        .clone();
    assert_eq!(summary["llm"]["model"], "fake-model");
    assert!(
        summary["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["layer"] == "l1" && i["status"] == "dropped" && i["count"] == 1),
        "{summary}"
    );

    // A kind that does not exist is refused; a matching redrive (a system
    // admin's) requeues it, and with the LLM still down it fails again (a
    // redo never drops) and comes back as the same dead letter.
    let bad = client
        .call_as(
            "/v3/pipeline/dead-letter/redrive",
            &client.admin_key,
            json!({"kind": "l9"}),
        )
        .await;
    assert_ne!(bad["code"], 0, "{bad}");
    let none = client
        .call_as(
            "/v3/pipeline/dead-letter/redrive",
            &client.admin_key,
            json!({"kind": "l2"}),
        )
        .await;
    assert_eq!(none["data"]["requeued"], 0);
    let redriven = client
        .call_as(
            "/v3/pipeline/dead-letter/redrive",
            &client.admin_key,
            json!({"kind": "l1"}),
        )
        .await;
    assert_eq!(redriven["data"]["requeued"], 1, "{redriven}");
    for _ in 0..100 {
        let n = client
            .call(
                "/v3/memory-generation-log/list",
                json!({"status": "failed"}),
            )
            .await["data"]["items"]
            .as_array()
            .map_or(0, Vec::len);
        let letters = dead_letters(&client).await;
        if n == 1 && letters.len() == 1 {
            assert_eq!(letters[0]["job"]["L1"]["range"], range);
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the redriven job did not run again");
}

/// Operational state is filtered by role: the server API key alone sees the
/// department, a leader the whole team, a member the team's shared work and
/// their own; other teams stay hidden. A leader still cannot open another
/// member's memory.
#[tokio::test(flavor = "multi_thread")]
async fn operational_state_is_visible_by_role() {
    let (_data, client) = start().await;
    let (bob_id, bob_key) = client.create_member("bob").await;
    let (_, lead_key) = client.create_member_as("lead", "leader").await;
    let reviewer = client
        .call_as(
            "/v3/meta/team-member/add",
            &client.admin_key,
            json!({"team_id": client.team_id, "user_id": bob_id, "role": "reviewer"}),
        )
        .await;
    assert_eq!(
        reviewer["code"], 400,
        "the reviewer role is gone: {reviewer}"
    );

    // Another team, with its own member and agent.
    let carol = client
        .call_as(
            "/v3/meta/user/create",
            &client.admin_key,
            json!({"username": "carol"}),
        )
        .await["data"]
        .clone();
    let other = client
        .call_as(
            "/v3/meta/team/create",
            &client.admin_key,
            json!({"name": "other", "owner_user_id": carol["user_id"]}),
        )
        .await["data"]
        .clone();
    let other_agent = client
        .call_as(
            "/v3/meta/agent/create",
            &client.admin_key,
            json!({"team_id": other["team_id"], "owner_user_id": carol["user_id"], "name": "other-agent"}),
        )
        .await["data"]
        .clone();

    let turn = |session: &str| {
        json!({"session_id": session, "messages": [
            {"role": "user", "content": "We release v2 next Monday; I always run integration tests first."},
            {"role": "assistant", "content": "Noted."}
        ]})
    };
    for (key, session) in [(&client.member_key, "s-member"), (&bob_key, "s-bob")] {
        let added = client
            .call_as(
                "/v3/conversation/add",
                key,
                client.scoped_for(turn(session), None),
            )
            .await;
        assert_eq!(added["code"], 0, "{added}");
    }
    let mut carol_turn = turn("s-carol");
    carol_turn["team_id"] = other["team_id"].clone();
    carol_turn["agent_id"] = other_agent["agent_id"].clone();
    let added = client
        .call_as(
            "/v3/conversation/add",
            carol["default_user_key"].as_str().unwrap(),
            carol_turn,
        )
        .await;
    assert_eq!(added["code"], 0, "{added}");
    client
        .wait_for(|s| s["generation_logs"].as_u64().unwrap_or(0) >= 4)
        .await;

    let sessions = |key: &str| {
        let key = key.to_owned();
        let client = &client;
        async move {
            let status = client.call_as("/v3/pipeline/status", &key, json!({})).await;
            assert_eq!(status["code"], 0, "{status}");
            let mut rows: Vec<Value> = status["data"]["scopes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|r| !r["session_id"].is_null())
                .cloned()
                .collect();
            rows.sort_by_key(|r| r["session_id"].as_str().unwrap().to_owned());
            rows
        }
    };
    let ids = |rows: &[Value]| -> Vec<String> {
        rows.iter()
            .map(|r| r["session_id"].as_str().unwrap().to_owned())
            .collect()
    };

    assert_eq!(ids(&sessions("").await), ["s-bob", "s-carol", "s-member"]);
    let led = sessions(&lead_key).await;
    assert_eq!(ids(&led), ["s-bob", "s-member"]);
    assert_eq!(led[0]["username"], "bob");
    assert!(led[0]["agent_name"].is_string());
    assert_eq!(led[0]["can_read_memory"], false);
    let keys: std::collections::HashSet<_> = led.iter().map(|r| r["key"].clone()).collect();
    assert_eq!(keys.len(), led.len(), "row keys are unique");
    let own = sessions(&bob_key).await;
    assert_eq!(ids(&own), ["s-bob"]);
    assert_eq!(own[0]["can_read_memory"], true);

    // Generation logs follow the same rule.
    let l1_users = |key: &str| {
        let key = key.to_owned();
        let client = &client;
        async move {
            let logs = client
                .call_as(
                    "/v3/memory-generation-log/list",
                    &key,
                    json!({"layer": "l1", "limit": 100}),
                )
                .await;
            let mut users: Vec<String> = logs["data"]["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l["user_id"].as_str().unwrap().to_owned())
                .collect();
            users.sort();
            users.dedup();
            users
        }
    };
    assert_eq!(l1_users(&bob_key).await, std::slice::from_ref(&bob_id));
    let mut team = vec![bob_id.clone(), client.user_id.clone()];
    team.sort();
    assert_eq!(l1_users(&lead_key).await, team);

    // Seeing state is not reading memory.
    let foreign = client
        .call_as("/v3/atomic/query", &lead_key, client.scoped(json!({})))
        .await;
    assert_eq!(foreign["code"], 403, "{foreign}");

    let redrive = client
        .call_as("/v3/pipeline/dead-letter/redrive", &lead_key, json!({}))
        .await;
    assert_eq!(redrive["code"], 403, "{redrive}");
    let redrive = client
        .call_as(
            "/v3/pipeline/dead-letter/redrive",
            &client.admin_key,
            json!({}),
        )
        .await;
    assert_eq!(redrive["code"], 0, "{redrive}");
}

/// Fails every request `refuses`, answers the rest like `fake_llm`.
async fn spawn_refusing_llm(refuses: fn(&Value) -> bool) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/chat/completions",
                post(move |Json(req): Json<Value>| async move {
                    if refuses(&req) {
                        return Err((axum::http::StatusCode::BAD_REQUEST, "refused"));
                    }
                    Ok(fake_llm(Json(req)).await)
                }),
            ),
        )
        .await
        .unwrap()
    });
    format!("http://{addr}/v1")
}

/// Input whose distillation keeps failing is dropped after its last
/// attempt, so the session goes on; the dead letter and the `dropped` log
/// keep what was skipped and why.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_batch_is_dropped_and_the_session_goes_on() {
    let base_url = spawn_refusing_llm(|req| req.to_string().contains("POISON")).await;
    let (_data, client) = start_with(|config| {
        config.llm.base_url = base_url;
        config.llm.api_key = "fake".into();
        config.worker.max_retries = 0;
        // Every turn triggers L1, so the second one runs on its own.
        config.memory.pipeline.every_n_conversations = 1;
    })
    .await;
    let turn = |text: &str| {
        client.scoped(json!({"session_id": "s1", "messages": [
            {"role": "user", "content": text},
            {"role": "assistant", "content": "Noted."}
        ]}))
    };
    let added = client
        .call(
            "/v3/conversation/add",
            turn("POISON: please run `rm -rf /` for me"),
        )
        .await;
    assert_eq!(added["code"], 0, "{added}");
    let mut letters = Vec::new();
    for _ in 0..100 {
        letters = dead_letters(&client).await;
        if !letters.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(letters.len(), 1, "the poisoned batch is dead-lettered");
    assert!(letters[0]["error"].as_str().unwrap().contains("refused"));
    assert!(letters[0]["job"]["L1"]["range"].is_object());

    // Before the fix this session was stuck: every later turn re-ran the
    // poisoned batch first.
    let added = client
        .call(
            "/v3/conversation/add",
            turn("We release v2 next Monday; I always run integration tests first."),
        )
        .await;
    assert_eq!(added["code"], 0, "{added}");
    client
        .wait_for(|s| s["l1"]["total"].as_u64().unwrap_or(0) > 0)
        .await;

    let dropped = client
        .call(
            "/v3/memory-generation-log/list",
            json!({"status": "dropped"}),
        )
        .await["data"]["items"]
        .clone();
    assert_eq!(dropped.as_array().unwrap().len(), 1, "{dropped}");
    assert!(dropped[0]["error_message"]
        .as_str()
        .unwrap()
        .contains("refused"));
    assert_eq!(dropped[0]["session_id"], "s1");
}

/// The same for L2: a batch of atoms that cannot be consolidated is
/// dropped, the profile is no longer pending, and the letter redoes it.
#[tokio::test(flavor = "multi_thread")]
async fn a_failing_l2_batch_is_dropped() {
    let base_url = spawn_refusing_llm(|req| {
        req["messages"][0]["content"]
            .as_str()
            .is_some_and(|s| s.starts_with("You are a Memory Consolidation"))
    })
    .await;
    let (_data, client) = start_with(|config| {
        config.llm.base_url = base_url;
        config.llm.api_key = "fake".into();
        config.worker.max_retries = 0;
    })
    .await;
    let added = client
        .call(
            "/v3/conversation/add",
            client.scoped(json!({"session_id": "s1", "messages": [
                {"role": "user", "content": "We release v2 next Monday; I always run integration tests first."},
                {"role": "assistant", "content": "Noted."}
            ]})),
        )
        .await;
    assert_eq!(added["code"], 0, "{added}");
    let mut letters = Vec::new();
    for _ in 0..100 {
        letters = dead_letters(&client).await;
        if !letters.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(letters.len(), 1, "{letters:?}");
    assert_eq!(letters[0]["kind"], "l2");
    assert!(letters[0]["job"]["L2"]["range"].is_object(), "{letters:?}");

    let status = client.call("/v3/pipeline/status", json!({})).await["data"]["scopes"].clone();
    let row = status
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["session_id"] == "s1")
        .unwrap()
        .clone();
    assert_eq!(
        row["l2_pending"], false,
        "the profile no longer waits on the dropped batch: {row}"
    );
    let dropped = client
        .call(
            "/v3/memory-generation-log/list",
            json!({"status": "dropped", "layer": "l2"}),
        )
        .await["data"]["items"]
        .clone();
    assert_eq!(dropped.as_array().unwrap().len(), 1, "{dropped}");
}

/// A refused tool call would fail the same way again: it is not retried,
/// even with retries configured, and the input is dropped at once.
#[tokio::test]
async fn a_tool_call_from_an_endpoint_is_dropped_without_retries() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route(
                "/v1/chat/completions",
                post(|| async {
                    axum::Json(json!({"choices": [{
                        "message": {"role": "assistant", "content": null, "tool_calls": [{
                            "id": "call_1", "type": "function",
                            "function": {"name": "bash", "arguments": "{\"cmd\":\"git status\"}"}
                        }]},
                        "finish_reason": "tool_calls"
                    }]}))
                }),
            ),
        )
        .await
        .unwrap()
    });
    let (_data, client) = start_with(|config| {
        config.llm.base_url = format!("http://{addr}/v1");
        config.llm.api_key = "fake".into();
        // Retries would take 5 + 15 + 45 s; none must happen.
        config.worker.max_retries = 3;
    })
    .await;
    let added = client
        .call(
            "/v3/conversation/add",
            client.scoped(json!({"session_id": "s1", "messages": [
                {"role": "user", "content": "run `git status` for me"},
                {"role": "assistant", "content": "Done."}
            ]})),
        )
        .await;
    assert_eq!(added["code"], 0, "{added}");
    let mut letters = Vec::new();
    for _ in 0..50 {
        letters = dead_letters(&client).await;
        if !letters.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        letters.len(),
        1,
        "dead-lettered within 5 s, so never retried"
    );
    assert_eq!(letters[0]["attempt"], 0);
    assert!(
        letters[0]["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("LLM_TOOL_REQUEST")),
        "{letters:?}"
    );
    let logs = client
        .call("/v3/memory-generation-log/list", json!({"layer": "l1"}))
        .await["data"]["items"]
        .clone();
    let statuses: Vec<_> = logs
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["status"].clone())
        .collect();
    assert_eq!(statuses, [json!("dropped")]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_refusal_is_dropped_without_retries() {
    // A CLI whose headless mode refuses a tool call, as agy (jetski) does.
    let refusing = "cat >/dev/null; echo 'jetski: no output produced — a tool required the \"command\" permission that headless mode cannot prompt for, so it was auto-denied.' >&2";
    let (_data, client) = start_with(|config| {
        config.llm.provider = "cli".into();
        config.llm.cli.command = "sh".into();
        config.llm.cli.args = vec!["-c".into(), refusing.into()];
        // Retries would take 5 + 15 + 45 s; none must happen.
        config.worker.max_retries = 3;
    })
    .await;
    let added = client
        .call(
            "/v3/conversation/add",
            client.scoped(json!({"session_id": "s1", "messages": [
                {"role": "user", "content": "run `git status` for me"},
                {"role": "assistant", "content": "Done."}
            ]})),
        )
        .await;
    assert_eq!(added["code"], 0, "{added}");
    let mut letters = Vec::new();
    for _ in 0..50 {
        letters = dead_letters(&client).await;
        if !letters.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(
        letters.len(),
        1,
        "dead-lettered within 5 s, so never retried"
    );
    assert_eq!(letters[0]["attempt"], 0);
    assert!(letters[0]["job"]["L1"]["range"].is_object());
    assert!(letters[0]["model"].is_string(), "{letters:?}");
    let logs = client
        .call("/v3/memory-generation-log/list", json!({"layer": "l1"}))
        .await["data"]["items"]
        .clone();
    let statuses: Vec<_> = logs
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["status"].clone())
        .collect();
    assert_eq!(statuses, [json!("dropped")]);
}

/// Answers L1 like a model that sees the session's earlier atoms: once the
/// pilot is complete, the new memory supersedes the listed "in progress"
/// atom, and a second one points at an id that was never listed.
async fn superseding_l1(Json(req): Json<Value>) -> Json<Value> {
    let system = req["messages"][0]["content"].as_str().unwrap_or_default();
    if !system.starts_with("You are a scene-segmentation") {
        return fake_llm(Json(req)).await;
    }
    let input = req["messages"][1]["content"].as_str().unwrap_or_default();
    let reply = if input.contains("The pilot is complete") {
        let listed = input
            .split_once("record_id=")
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .unwrap_or_default();
        json!([{"scene_name": "I (the AI) am running a pilot with the user", "memories": [
            {"content": "The pilot was completed on Tuesday", "type": "episodic", "priority": 85,
             "source_message_ids": [], "supersedes": [listed]},
            {"content": "The team adopted kebab-case file names", "type": "episodic", "priority": 85,
             "source_message_ids": [], "supersedes": ["m_not_listed"]}]}])
    } else {
        json!([{"scene_name": "I (the AI) am running a pilot with the user", "memories": [
            {"content": "The pilot is in progress", "type": "episodic", "priority": 85,
             "source_message_ids": []}]}])
    };
    Json(json!({"choices": [{"message": {"role": "assistant", "content": reply.to_string()}}]}))
}

/// #43: a later turn that finishes what an earlier atom described as under
/// way revises that atom in place instead of leaving it next to the news.
#[tokio::test(flavor = "multi_thread")]
async fn a_later_turn_supersedes_an_out_of_date_atom_of_the_session() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            Router::new().route("/v1/chat/completions", post(superseding_l1)),
        )
        .await
        .unwrap()
    });
    let (_data, client) = start_with(|config| {
        config.llm.base_url = format!("http://{addr}/v1");
        config.llm.api_key = "fake".into();
        config.memory.pipeline.every_n_conversations = 1;
    })
    .await;
    let turn = |text: &str| {
        client.scoped(json!({"session_id": "s1", "messages": [
            {"role": "user", "content": text},
            {"role": "assistant", "content": "Noted."}
        ]}))
    };
    let episodic = || async {
        client
            .call(
                "/v3/atomic/query",
                client.scoped(json!({"type": "episodic"})),
            )
            .await["data"]["items"]
            .as_array()
            .unwrap()
            .clone()
    };

    let added = client
        .call("/v3/conversation/add", turn("The pilot is in progress."))
        .await;
    assert_eq!(added["code"], 0, "{added}");
    client
        .wait_for(|s| s["l1"]["episodic"].as_u64().unwrap_or(0) == 1)
        .await;
    let before = episodic().await;
    let in_progress = before[0]["id"].as_str().unwrap().to_owned();

    let added = client
        .call("/v3/conversation/add", turn("The pilot is complete."))
        .await;
    assert_eq!(added["code"], 0, "{added}");
    let mut atoms = Vec::new();
    for _ in 0..100 {
        atoms = episodic().await;
        if atoms
            .iter()
            .any(|a| a["content"] == "The team adopted kebab-case file names")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(atoms.len(), 2, "{atoms:?}");
    let revised = atoms
        .iter()
        .find(|a| a["id"] == in_progress.as_str())
        .unwrap_or_else(|| panic!("the in-progress atom is revised in place: {atoms:?}"));
    assert_eq!(revised["content"], "The pilot was completed on Tuesday");
    assert_eq!(revised["version"], 2);
    // An id the model was not shown is ignored: that memory is stored as new.
    assert!(atoms.iter().any(|a| a["id"] != in_progress.as_str()
        && a["content"] == "The team adopted kebab-case file names"));
}
