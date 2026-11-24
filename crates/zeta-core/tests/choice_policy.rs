//! The choice policy (plan 48, design 5.4): its default, its persistence and its checks.

use zeta_core::choice_policy::{Budget, Ceiling, ChoicePolicy, LeaseSetting, Surface, surface_of};
use zeta_core::node::Node;
use zeta_core::ops::{Op, OpError, OpResult};
use zeta_core::stage::StageRegistry;

mod common;

async fn open(dir: &std::path::Path) -> Node {
    Node::open(common::cfg(dir, false), StageRegistry::new(), vec![])
        .await
        .unwrap()
}

fn set_op(policy: ChoicePolicy) -> Op {
    Op::ChoicePolicySet {
        policy: Some(policy),
        lease: None,
    }
}

async fn get(node: &Node) -> ChoicePolicy {
    match node.apply(Op::ChoicePolicyGet {}).await {
        Ok(OpResult::ChoicePolicy(p)) => p,
        other => panic!("expected the policy, got {other:?}"),
    }
}

#[tokio::test]
async fn the_default_policy_is_the_specs() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let p = get(&node).await;
    let c = |default, cap| Ceiling { default, cap };
    assert_eq!(p.surfaces[&Surface::Menu], c(1, 2));
    assert_eq!(p.surfaces[&Surface::Watch], c(3, 3));
    assert_eq!(p.surfaces[&Surface::Agent], c(1, 3));
    assert_eq!(p.surfaces[&Surface::Client], c(1, 3));
    assert_eq!(p.lease, LeaseSetting::Share);
    assert_eq!(
        p.budget,
        Budget {
            trial_s: 30,
            concurrent: 3,
            slice_s: 2.0
        }
    );
    assert_eq!(p.confidence_stop, None);
    // The keys serialise as snake_case names.
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["surfaces"]["menu"]["cap"], 2);
}

#[tokio::test]
async fn a_set_policy_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut want = ChoicePolicy {
        lease: LeaseSetting::Widen,
        ..ChoicePolicy::default()
    };
    want.surfaces
        .insert(Surface::Menu, Ceiling { default: 2, cap: 3 });
    want.budget.slice_s = 1.5;
    {
        let node = open(dir.path()).await;
        let r = node.apply(set_op(want.clone())).await;
        assert_eq!(r, Ok(OpResult::ChoicePolicy(want.clone())));
        assert_eq!(get(&node).await, want);
    }
    let node = open(dir.path()).await;
    assert_eq!(get(&node).await, want);
}

#[tokio::test]
async fn a_default_above_its_cap_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let mut bad = ChoicePolicy::default();
    bad.surfaces
        .insert(Surface::Agent, Ceiling { default: 3, cap: 2 });
    match node.apply(set_op(bad)).await {
        Err(OpError::Other { message }) => assert!(message.contains("agent"), "{message}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let mut bad = ChoicePolicy::default();
    bad.surfaces
        .insert(Surface::Menu, Ceiling { default: 1, cap: 4 });
    assert!(node.apply(set_op(bad)).await.is_err());
    for edit in [
        (|p: &mut ChoicePolicy| p.budget.concurrent = 0) as fn(&mut ChoicePolicy),
        |p| p.budget.trial_s = 0,
        |p| p.budget.slice_s = 0.0,
    ] {
        let mut bad = ChoicePolicy::default();
        edit(&mut bad);
        assert!(node.apply(set_op(bad)).await.is_err());
    }
    // Nothing was kept.
    assert_eq!(get(&node).await, ChoicePolicy::default());
    assert!(!dir.path().join("state/choice-policy.json").exists());
}

#[test]
fn an_ask_above_the_cap_is_clamped_and_said() {
    let p = ChoicePolicy::default();
    assert_eq!(
        p.steps(Surface::Menu, Some(3)),
        (2, Some("not tried: capped at step 2 for menu".into()))
    );
    assert_eq!(p.steps(Surface::Menu, None), (1, None));
    assert_eq!(p.steps(Surface::Watch, None), (3, None));
    assert_eq!(p.steps(Surface::Agent, Some(2)), (2, None));
}

#[tokio::test]
async fn a_lease_alone_keeps_every_other_value_and_takes_effect() {
    let dir = tempfile::tempdir().unwrap();
    let mut custom = ChoicePolicy {
        confidence_stop: Some(0.9),
        ..ChoicePolicy::default()
    };
    custom
        .surfaces
        .insert(Surface::Menu, Ceiling { default: 2, cap: 3 });
    custom.budget = Budget {
        trial_s: 12,
        concurrent: 2,
        slice_s: 1.5,
    };
    let want = ChoicePolicy {
        lease: LeaseSetting::Widen,
        ..custom.clone()
    };
    {
        let node = open(dir.path()).await;
        node.apply(set_op(custom)).await.unwrap();
        let r = node
            .apply(Op::ChoicePolicySet {
                policy: None,
                lease: Some(LeaseSetting::Widen),
            })
            .await;
        assert_eq!(r, Ok(OpResult::ChoicePolicy(want.clone())));
        assert_eq!(get(&node).await, want);
    }
    // Persisted, not only held.
    let node = open(dir.path()).await;
    assert_eq!(get(&node).await, want);
    // Both, or neither, is refused.
    for (policy, lease) in [
        (Some(want.clone()), Some(LeaseSetting::Share)),
        (None, None),
    ] {
        let r = node.apply(Op::ChoicePolicySet { policy, lease }).await;
        assert!(matches!(r, Err(OpError::Other { .. })), "{r:?}");
    }
    assert_eq!(get(&node).await, want);
}

#[tokio::test]
async fn a_surface_left_out_takes_its_built_in_ceiling() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path()).await;
    let mut p = ChoicePolicy::default();
    p.surfaces.remove(&Surface::Menu);
    node.apply(set_op(p.clone())).await.unwrap();
    assert_eq!(get(&node).await, p);
    assert_eq!(
        p.steps(Surface::Menu, Some(3)),
        (2, Some("not tried: capped at step 2 for menu".into()))
    );
    assert_eq!(p.steps(Surface::Menu, None), (1, None));
}

#[test]
fn an_ask_of_zero_runs_step_one() {
    let p = ChoicePolicy::default();
    assert_eq!(p.steps(Surface::Agent, Some(0)), (1, None));
}

#[tokio::test]
async fn a_broken_policy_file_loads_as_the_default_with_a_load_error() {
    let mut bad_default = ChoicePolicy::default();
    bad_default
        .surfaces
        .insert(Surface::Agent, Ceiling { default: 3, cap: 2 });
    let files = [
        "{ not json".to_string(),
        serde_json::to_string(&bad_default).unwrap(),
    ];
    for text in files {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("state")).unwrap();
        std::fs::write(dir.path().join("state/choice-policy.json"), &text).unwrap();
        let node = open(dir.path()).await;
        assert_eq!(get(&node).await, ChoicePolicy::default(), "{text}");
        let (_, others) = node.load_errors_by_origin();
        assert!(
            others.iter().any(|e| e.contains("choice-policy.json")),
            "{others:?}"
        );
    }
}

#[test]
fn a_confidence_stop_outside_zero_to_one_is_refused() {
    for bad in [f32::NAN, -0.1, 1.5] {
        let p = ChoicePolicy {
            confidence_stop: Some(bad),
            ..ChoicePolicy::default()
        };
        assert!(p.validate().unwrap_err().contains("confidence_stop"));
    }
    for ok in [None, Some(0.0), Some(1.0)] {
        let p = ChoicePolicy {
            confidence_stop: ok,
            ..ChoicePolicy::default()
        };
        assert_eq!(p.validate(), Ok(()));
    }
}

#[test]
fn the_surface_follows_the_transport() {
    assert_eq!(surface_of("ui"), Surface::Menu);
    assert_eq!(surface_of("agent"), Surface::Agent);
    for t in ["local", "http", "cli"] {
        assert_eq!(surface_of(t), Surface::Client);
    }
}
