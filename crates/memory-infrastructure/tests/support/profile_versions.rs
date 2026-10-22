//! The `ProfileVersionStore` contract, run against each backend.

use std::collections::BTreeMap;

use memory_application::ports::memory::ProfileVersionStore;
use memory_domain::error::ErrorKind;
use memory_domain::profile::{
    NewPersonaVersion, NewSceneVersion, PersonaOp, ProfileScope, SceneOp, SceneRef,
};
use memory_domain::scope::DepartmentId;

fn version(scene_id: &str, op: SceneOp, slug: &str, body: &str) -> NewSceneVersion {
    NewSceneVersion {
        scene_id: scene_id.into(),
        op,
        slug: slug.into(),
        summary: format!("about {slug}"),
        heat: 1,
        body: body.into(),
        attrs: BTreeMap::new(),
        parents: Vec::new(),
        author: None,
        expected_version: None,
    }
}

fn persona(body: &str) -> NewPersonaVersion {
    NewPersonaVersion {
        op: PersonaOp::Generate,
        body: body.into(),
        attrs: BTreeMap::new(),
        author: None,
    }
}

pub async fn exercise(
    store: &dyn ProfileVersionStore,
    department: &DepartmentId,
    profile: &ProfileScope,
    other: &ProfileScope,
) {
    // Persona: append-only, identical bodies keep the version.
    let append = |body: &'static str| store.append_persona(department, profile, persona(body));
    assert_eq!(append("v1").await.unwrap().version, 1);
    assert_eq!(
        append("v1").await.unwrap().version,
        1,
        "unchanged body keeps version"
    );
    assert_eq!(append("v2").await.unwrap().version, 2);
    let old = store
        .persona(department, profile, Some(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!((old.body.as_str(), old.op), ("v1", PersonaOp::Generate));
    assert_eq!(
        store
            .persona(department, profile, None)
            .await
            .unwrap()
            .unwrap()
            .version,
        2
    );
    assert_eq!(store.count_personas(department, None).await.unwrap(), 1);

    // Scenes: create, an identical write, an update.
    let append = |v: Vec<NewSceneVersion>| store.append_scenes(department, profile, v);
    append(vec![
        version("scn-a", SceneOp::Create, "a.md", "a1"),
        version("scn-b", SceneOp::Create, "b.md", "b1"),
    ])
    .await
    .unwrap();
    let same = append(vec![version("scn-a", SceneOp::Update, "a.md", "a1")])
        .await
        .unwrap();
    assert_eq!(same[0].version, 1, "unchanged content keeps version");
    let a2 = append(vec![version("scn-a", SceneOp::Update, "a.md", "a2")])
        .await
        .unwrap();
    assert_eq!(a2[0].version, 2);
    assert_eq!(
        store
            .scene_heads(department, profile, "")
            .await
            .unwrap()
            .len(),
        2
    );
    let a1 = store
        .scene(department, profile, "a.md", Some(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(a1.body, "a1", "old versions stay readable");

    // Optimistic lock: a writer that saw an older version is refused.
    let mut stale = version("scn-a", SceneOp::Update, "a.md", "stale");
    stale.expected_version = Some(1);
    let err = append(vec![stale]).await.unwrap_err();
    assert_eq!(
        (err.kind, err.code.as_str()),
        (ErrorKind::Conflict, "scene_changed")
    );
    let mut fresh = version("scn-a", SceneOp::Update, "a.md", "a2");
    fresh.expected_version = Some(2);
    assert_eq!(
        append(vec![fresh]).await.unwrap()[0].version,
        2,
        "on top of v2, unchanged"
    );

    // A live slug cannot be claimed by another scene.
    let err = append(vec![version("scn-c", SceneOp::Create, "a.md", "x")])
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::Conflict);

    // Merge: a absorbs b, which is retired but keeps its versions.
    let mut merged = version("scn-a", SceneOp::Merge, "a.md", "a+b");
    merged.parents = vec![SceneRef {
        scene_id: "scn-b".into(),
        version: 1,
    }];
    let mut retired = version("scn-b", SceneOp::Retire, "b.md", "b1");
    retired.attrs.insert("merged_into".into(), "scn-a".into());
    let written = append(vec![merged, retired]).await.unwrap();
    assert_eq!((written[0].version, written[1].version), (3, 2));
    let heads = store.scene_heads(department, profile, "").await.unwrap();
    assert_eq!(heads.len(), 1);
    assert_eq!(heads[0].parents[0].scene_id, "scn-b");
    let gone = store
        .scene(department, profile, "b.md", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(gone.op, SceneOp::Retire);
    assert_eq!(gone.attrs["merged_into"], "scn-a");
    assert_eq!(store.count_scenes(department, None, "").await.unwrap(), 1);

    // Retiring a retired scene stores nothing.
    let again = append(vec![version("scn-b", SceneOp::Retire, "b.md", "b1")])
        .await
        .unwrap();
    assert_eq!(again[0].version, 2);

    // Retiring with unchanged content (a removal) is still a new version.
    append(vec![version("scn-e", SceneOp::Create, "e.md", "e1")])
        .await
        .unwrap();
    let removed = append(vec![version("scn-e", SceneOp::Retire, "e.md", "e1")])
        .await
        .unwrap();
    assert_eq!((removed[0].version, removed[0].op), (2, SceneOp::Retire));
    assert_eq!(store.count_scenes(department, None, "").await.unwrap(), 1);

    // A retired slug can be reused; the live scene wins the name.
    append(vec![version("scn-d", SceneOp::Create, "b.md", "new b")])
        .await
        .unwrap();
    let b = store
        .scene(department, profile, "b.md", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((b.scene_id.as_str(), b.body.as_str()), ("scn-d", "new b"));
    // By id, the old scene stays reachable after losing its name.
    let old_b = store
        .scene_by_id(department, profile, "scn-b", Some(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old_b.body, "b1");
    let last_b = store
        .scene_by_id(department, profile, "scn-b", None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!((last_b.version, last_b.op), (2, SceneOp::Retire));
    assert!(store
        .scene_by_id(department, other, "scn-b", None)
        .await
        .unwrap()
        .is_none());

    // Scene ids belong to one profile.
    let err = store
        .append_scenes(
            department,
            other,
            vec![version("scn-a", SceneOp::Update, "a.md", "x")],
        )
        .await
        .unwrap_err();
    assert_eq!(err.kind, ErrorKind::InvalidInput);

    // Purge is the one way history goes away: 4 scenes + 1 persona.
    assert_eq!(store.purge(department, profile).await.unwrap(), 5);
    assert!(store
        .scene_heads(department, profile, "")
        .await
        .unwrap()
        .is_empty());
    assert!(store
        .scene(department, profile, "b.md", Some(1))
        .await
        .unwrap()
        .is_none());
    assert!(store
        .persona(department, profile, None)
        .await
        .unwrap()
        .is_none());
    assert!(
        store
            .scene_by_id(department, profile, "scn-b", Some(1))
            .await
            .unwrap()
            .is_none(),
        "versions go with their scene"
    );
}
