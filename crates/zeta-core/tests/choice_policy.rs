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
        let r = node
            .apply(Op::ChoicePolicySet {
                policy: want.clone(),
            })
            .await;
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
    match node.apply(Op::ChoicePolicySet { policy: bad }).await {
        Err(OpError::Other { message }) => assert!(message.contains("agent"), "{message}"),
        other => panic!("expected a refusal, got {other:?}"),
    }
    let mut bad = ChoicePolicy::default();
    bad.surfaces
        .insert(Surface::Menu, Ceiling { default: 1, cap: 4 });
    assert!(
        node.apply(Op::ChoicePolicySet { policy: bad })
            .await
            .is_err()
    );
    for edit in [
        (|p: &mut ChoicePolicy| p.budget.concurrent = 0) as fn(&mut ChoicePolicy),
        |p| p.budget.trial_s = 0,
        |p| p.budget.slice_s = 0.0,
    ] {
        let mut bad = ChoicePolicy::default();
        edit(&mut bad);
        assert!(
            node.apply(Op::ChoicePolicySet { policy: bad })
                .await
                .is_err()
        );
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

#[test]
fn the_surface_follows_the_transport() {
    assert_eq!(surface_of("ui"), Surface::Menu);
    assert_eq!(surface_of("agent"), Surface::Agent);
    for t in ["local", "http", "cli"] {
        assert_eq!(surface_of(t), Surface::Client);
    }
}
