//! Which atoms a scene was built from and why it may be stale, run against
//! each backend: the marks atom writes leave, how L2 writes, merges,
//! removals and manual writes move and settle them, and that one profile
//! never reaches another's.

use std::collections::BTreeMap;

use memory_application::ports::memory::{AtomRepository, ProfileVersionStore};
use memory_domain::memory::AtomicMemory;
use memory_domain::profile::{
    MemberState, NewSceneVersion, ProfileScope, SceneMember, SceneOp, SceneRef, SceneWriteLinks,
    StaleMark, StaleReason,
};
use memory_domain::scope::{DepartmentId, MemoryScope};

fn version(scene_id: &str, op: SceneOp, slug: &str) -> NewSceneVersion {
    NewSceneVersion {
        scene_id: scene_id.into(),
        op,
        slug: slug.into(),
        summary: format!("about {slug}"),
        heat: 1,
        body: format!("{slug} at {}", op.as_str()),
        attrs: BTreeMap::new(),
        parents: Vec::new(),
        author: None,
        expected_version: None,
    }
}

fn member(atom: &AtomicMemory) -> SceneMember {
    SceneMember {
        atom_id: atom.id.clone(),
        atom_version: atom.version,
        user_id: atom.scope.user_id.to_string(),
        content: atom.content.clone(),
    }
}

fn links(members: Vec<SceneMember>, resolved: Vec<i64>) -> SceneWriteLinks {
    SceneWriteLinks { members, resolved }
}

fn reasons(marks: &[StaleMark]) -> Vec<(&str, &str, StaleReason)> {
    marks
        .iter()
        .map(|m| (m.scene_id.as_str(), m.atom_id.as_str(), m.reason))
        .collect()
}

pub async fn exercise<S>(
    store: &S,
    department: &DepartmentId,
    scope: &MemoryScope,
    other: &ProfileScope,
    atom: impl Fn(&str, &str) -> AtomicMemory,
) where
    S: AtomRepository + ProfileVersionStore,
{
    let profile = ProfileScope::from(scope);
    let marks = || store.stale_marks(department, &profile, None);
    let mut a1 = atom("a1", "deploy with deploy.sh");
    let a2 = atom("a2", "staging is rebuilt on Fridays");
    for a in [&a1, &a2] {
        store.upsert(department, a, None).await.unwrap();
    }

    // L2 places a1 and a2 in scene X: nothing is stale.
    store
        .append_scenes_linked(
            department,
            &profile,
            vec![version("scn-x", SceneOp::Create, "deploy.md")],
            links(vec![member(&a1), member(&a2)], vec![]),
        )
        .await
        .unwrap();
    let rows = store
        .scene_atoms(department, &profile, "scn-x")
        .await
        .unwrap();
    assert_eq!(rows.len(), 2);
    assert!(rows
        .iter()
        .all(|r| r.state == MemberState::Current && r.atom_version == 1));
    assert!(marks().await.unwrap().is_empty());

    // Rewrites: one mark per scene and atom, the oldest text kept.
    a1.content = "deploy with deploy_prod.sh".into();
    a1.version = 2;
    store.upsert(department, &a1, None).await.unwrap();
    a1.content = "deploy with deploy_prod.sh --canary".into();
    a1.version = 3;
    store.upsert(department, &a1, None).await.unwrap();
    // Writing the same version again (an embedding, say) marks nothing.
    store.upsert(department, &a1, None).await.unwrap();
    let open = marks().await.unwrap();
    assert_eq!(reasons(&open), [("scn-x", "a1", StaleReason::Rewritten)]);
    assert_eq!(open[0].atom_version, Some(3));
    assert_eq!(
        open[0].before_content.as_deref(),
        Some("deploy with deploy.sh")
    );
    store
        .note_stale_attempt(department, &profile, &[open[0].id])
        .await
        .unwrap();
    assert_eq!(marks().await.unwrap()[0].attempts, 1);

    // L2 places a1 again at the version it now has: settled.
    store
        .append_scenes_linked(
            department,
            &profile,
            vec![version("scn-x", SceneOp::Update, "deploy.md")],
            links(vec![member(&a1)], vec![]),
        )
        .await
        .unwrap();
    assert!(marks().await.unwrap().is_empty());

    // Deleting a2 marks X with its text; L2 settles it by id.
    store
        .delete(department, scope, &["a2".to_owned()])
        .await
        .unwrap();
    let open = marks().await.unwrap();
    assert_eq!(reasons(&open), [("scn-x", "a2", StaleReason::Deleted)]);
    assert_eq!(
        open[0].before_content.as_deref(),
        Some("staging is rebuilt on Fridays")
    );
    assert_eq!(open[0].atom_version, None);
    // Another profile cannot settle it.
    store
        .append_scenes_linked(department, other, vec![], links(vec![], vec![open[0].id]))
        .await
        .unwrap();
    assert_eq!(marks().await.unwrap().len(), 1);
    assert!(store
        .stale_marks(department, other, None)
        .await
        .unwrap()
        .is_empty());
    store
        .append_scenes_linked(
            department,
            &profile,
            vec![],
            links(vec![], vec![open[0].id]),
        )
        .await
        .unwrap();
    assert!(marks().await.unwrap().is_empty());

    // L2 places a1 in scene Y: X keeps a moved-out row and is marked.
    store
        .append_scenes_linked(
            department,
            &profile,
            vec![version("scn-y", SceneOp::Create, "release.md")],
            links(vec![member(&a1)], vec![]),
        )
        .await
        .unwrap();
    let x = store
        .scene_atoms(department, &profile, "scn-x")
        .await
        .unwrap();
    let a1_in_x = x.iter().find(|r| r.atom_id == "a1").unwrap();
    assert_eq!(a1_in_x.state, MemberState::MovedOut);
    let y = store
        .scene_atoms(department, &profile, "scn-y")
        .await
        .unwrap();
    assert_eq!(
        (y[0].atom_id.as_str(), y[0].state),
        ("a1", MemberState::Current)
    );
    assert_eq!(
        reasons(&marks().await.unwrap()),
        [("scn-x", "a1", StaleReason::MovedOut)]
    );

    // A manual write settles everything X had.
    let mut manual = version("scn-x", SceneOp::Manual, "deploy.md");
    manual.author = Some("alice".into());
    store
        .append_scenes(department, &profile, vec![manual])
        .await
        .unwrap();
    assert!(marks().await.unwrap().is_empty());

    // L2 read a3 at v1 while it was rewritten to v2: Y is marked.
    let mut a3 = atom("a3", "release notes go out on Tuesdays");
    let seen = member(&a3);
    a3.version = 2;
    a3.content = "release notes go out on Wednesdays".into();
    store.upsert(department, &a3, None).await.unwrap();
    store
        .append_scenes_linked(
            department,
            &profile,
            vec![version("scn-y", SceneOp::Update, "release.md")],
            links(vec![seen], vec![]),
        )
        .await
        .unwrap();
    let open = marks().await.unwrap();
    assert_eq!(
        reasons(&open),
        [("scn-y", "a3", StaleReason::ChangedDuringRun)]
    );
    assert_eq!(open[0].atom_version, Some(2));
    assert_eq!(
        open[0].before_content.as_deref(),
        Some("release notes go out on Tuesdays")
    );

    // Z merges Y: Y's atoms and open marks follow.
    let y_latest = store
        .scene_by_id(department, &profile, "scn-y", None)
        .await
        .unwrap()
        .unwrap();
    let mut merge = version("scn-z", SceneOp::Merge, "ops.md");
    merge.parents = vec![SceneRef {
        scene_id: "scn-y".into(),
        version: y_latest.version,
    }];
    let mut retire = version("scn-y", SceneOp::Retire, "release.md");
    retire.attrs.insert("merged_into".into(), "scn-z".into());
    store
        .append_scenes(department, &profile, vec![merge, retire])
        .await
        .unwrap();
    let z = store
        .scene_atoms(department, &profile, "scn-z")
        .await
        .unwrap();
    let mut z_atoms: Vec<_> = z.iter().map(|r| (r.atom_id.as_str(), r.state)).collect();
    z_atoms.sort_by_key(|(id, _)| *id);
    assert_eq!(
        z_atoms,
        [("a1", MemberState::Current), ("a3", MemberState::Current)]
    );
    assert!(store
        .scene_atoms(department, &profile, "scn-y")
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        reasons(&marks().await.unwrap()),
        [("scn-z", "a3", StaleReason::ChangedDuringRun)]
    );

    // Removing Z moves its atoms out; nothing is current any more.
    store
        .append_scenes(
            department,
            &profile,
            vec![version("scn-z", SceneOp::Retire, "ops.md")],
        )
        .await
        .unwrap();
    assert!(store
        .scene_atoms(department, &profile, "scn-z")
        .await
        .unwrap()
        .iter()
        .all(|r| r.state == MemberState::MovedOut));
    // a1 belongs nowhere, so rewriting it marks nothing new.
    a1.version = 4;
    store.upsert(department, &a1, None).await.unwrap();
    assert_eq!(marks().await.unwrap().len(), 1);

    // Purging the profile takes rows and marks with the scenes.
    store.purge(department, &profile).await.unwrap();
    assert!(marks().await.unwrap().is_empty());
    assert!(store
        .scene_atoms(department, &profile, "scn-x")
        .await
        .unwrap()
        .is_empty());
}
