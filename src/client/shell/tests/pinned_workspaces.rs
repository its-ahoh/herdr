use super::*;

fn snapshot_with_workspaces(ids: &[&str]) -> ClientShellSnapshot {
    let mut projected = snapshot();
    let template = projected.workspaces[0].clone();
    projected.workspaces = ids
        .iter()
        .enumerate()
        .map(|(index, id)| ClientShellWorkspace {
            workspace_id: (*id).to_owned(),
            number: index + 1,
            label: (*id).to_owned(),
            focused: index == 0,
            ..template.clone()
        })
        .collect();
    projected
}

fn state_with_workspaces(ids: &[&str]) -> ClientShellState {
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.set_snapshot(Box::new(snapshot_with_workspaces(ids)));
    state
}

fn moved_workspace(outcome: &ClientShellInput) -> Option<(String, usize)> {
    match &outcome.actions[..] {
        [ClientShellAction::Endpoint { request, .. }] => match &request.method {
            crate::api::schema::Method::WorkspaceMove(params) => {
                Some((params.workspace_id.clone(), params.insert_index))
            }
            _ => None,
        },
        _ => None,
    }
}

#[test]
fn pin_and_unpin_preserve_the_pinned_block_order() {
    let mut state = state_with_workspaces(&["ws_1", "ws_2", "ws_3"]);
    let mut outcome = ClientShellInput::default();

    state.toggle_workspace_pin("ws_2".into(), &mut outcome);
    assert_eq!(state.pinned_workspaces, HashSet::from(["ws_2".to_owned()]));
    assert!(matches!(
        &outcome.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceMove(params)
                    if params.workspace_id == "ws_2" && params.insert_index == 0
            )
    ));

    // Apply the server's reordered snapshot, then pin another workspace. It is
    // inserted after the existing pinned workspace rather than ahead of it.
    state = state_with_workspaces(&["ws_2", "ws_1", "ws_3"]);
    state.pinned_workspaces.insert("ws_2".into());
    outcome = ClientShellInput::default();
    state.toggle_workspace_pin("ws_3".into(), &mut outcome);
    assert_eq!(
        state.pinned_workspaces,
        HashSet::from(["ws_2".to_owned(), "ws_3".to_owned()])
    );
    assert!(matches!(
        &outcome.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceMove(params)
                    if params.workspace_id == "ws_3" && params.insert_index == 1
            )
    ));

    // Once the server reflects that move, unpinning the first workspace places
    // it at the start of the unpinned block, after the remaining pin.
    state = state_with_workspaces(&["ws_2", "ws_3", "ws_1"]);
    state.pinned_workspaces = HashSet::from(["ws_2".to_owned(), "ws_3".to_owned()]);
    outcome = ClientShellInput::default();
    state.toggle_workspace_pin("ws_2".into(), &mut outcome);
    assert_eq!(state.pinned_workspaces, HashSet::from(["ws_3".to_owned()]));
    assert!(matches!(
        &outcome.actions[..],
        [ClientShellAction::Endpoint { request, .. }]
            if matches!(
                &request.method,
                crate::api::schema::Method::WorkspaceMove(params)
                    if params.workspace_id == "ws_2" && params.insert_index == 2
            )
    ));
}

#[test]
fn drag_targets_are_clamped_to_the_pinned_boundary() {
    let mut state = state_with_workspaces(&["pinned_1", "pinned_2", "regular_1", "regular_2"]);
    state.pinned_workspaces = HashSet::from(["pinned_1".to_owned(), "pinned_2".to_owned()]);

    assert_eq!(
        state.clamp_workspace_drop_target("regular_2", Some("pinned_1".into())),
        Some("regular_1".into())
    );
    assert_eq!(
        state.clamp_workspace_drop_target("pinned_1", Some("regular_2".into())),
        Some("regular_1".into())
    );
    assert_eq!(
        state.clamp_workspace_drop_target("pinned_1", Some("pinned_2".into())),
        Some("pinned_2".into())
    );
}

#[test]
fn closed_pinned_workspace_does_not_pin_a_reused_id() {
    let mut state = state_with_workspaces(&["w1", "w2"]);
    state.pinned_workspaces.insert("w2".into());

    // w2 is closed: the next snapshot forgets its pin.
    state.set_snapshot(Box::new(snapshot_with_workspaces(&["w1"])));
    state.reconcile_pinned_workspaces();
    assert!(state.pinned_workspaces.is_empty());

    // A later workspace that reuses the id w2 starts unpinned.
    state.set_snapshot(Box::new(snapshot_with_workspaces(&["w1", "w2"])));
    let outcome = state.reconcile_pinned_workspaces();
    assert!(!state.workspace_is_pinned(&ClientEndpointId::Local, "w2"));
    assert!(outcome.actions.is_empty());
}

#[test]
fn empty_workspace_list_keeps_pins() {
    let mut state = state_with_workspaces(&["w1", "w2"]);
    state.pinned_workspaces.insert("w2".into());

    state.set_snapshot(Box::new(snapshot_with_workspaces(&[])));
    state.reconcile_pinned_workspaces();
    assert_eq!(state.pinned_workspaces, HashSet::from(["w2".to_owned()]));
}

#[test]
fn snapshot_with_misplaced_pin_sends_one_repair_move() {
    let mut state = state_with_workspaces(&["w1", "w2", "w3"]);
    state.pinned_workspaces.insert("w3".into());

    let outcome = state.reconcile_pinned_workspaces();
    assert_eq!(moved_workspace(&outcome), Some(("w3".to_owned(), 0)));

    // The same wrong order again (move not applied yet, or rejected) does not
    // resend the move.
    state.set_snapshot(Box::new(snapshot_with_workspaces(&["w1", "w2", "w3"])));
    assert!(state.reconcile_pinned_workspaces().actions.is_empty());

    // Once the order is right, nothing is sent.
    state.set_snapshot(Box::new(snapshot_with_workspaces(&["w3", "w1", "w2"])));
    assert!(state.reconcile_pinned_workspaces().actions.is_empty());

    // A new wrong order (for example after a drag from another client) is
    // repaired again.
    state.set_snapshot(Box::new(snapshot_with_workspaces(&["w1", "w3", "w2"])));
    assert_eq!(
        moved_workspace(&state.reconcile_pinned_workspaces()),
        Some(("w3".to_owned(), 0))
    );
}

#[test]
fn toggling_a_pin_does_not_trigger_a_duplicate_repair() {
    let mut state = state_with_workspaces(&["w1", "w2", "w3"]);
    let mut outcome = ClientShellInput::default();
    state.toggle_workspace_pin("w3".into(), &mut outcome);
    assert_eq!(moved_workspace(&outcome), Some(("w3".to_owned(), 0)));

    // A snapshot that still shows the old order arrives before the move lands.
    state.set_snapshot(Box::new(snapshot_with_workspaces(&["w1", "w2", "w3"])));
    assert!(state.reconcile_pinned_workspaces().actions.is_empty());
}

#[test]
fn two_clients_with_different_pins_stop_after_one_round() {
    // Another client pins w2 and keeps moving it first; this client pins w3.
    let mut state = state_with_workspaces(&["w2", "w3", "w1"]);
    state.pinned_workspaces.insert("w3".into());

    state.set_snapshot(Box::new(snapshot_with_workspaces(&["w2", "w3", "w1"])));
    assert_eq!(
        moved_workspace(&state.reconcile_pinned_workspaces()),
        Some(("w3".to_owned(), 0))
    );

    // The orders keep taking turns; each one was already repaired once.
    state.set_snapshot(Box::new(snapshot_with_workspaces(&["w3", "w2", "w1"])));
    assert!(state.reconcile_pinned_workspaces().actions.is_empty());
    state.set_snapshot(Box::new(snapshot_with_workspaces(&["w2", "w3", "w1"])));
    assert!(state.reconcile_pinned_workspaces().actions.is_empty());

    // Changing pins starts fresh.
    let mut outcome = ClientShellInput::default();
    state.toggle_workspace_pin("w1".into(), &mut outcome);
    assert!(!outcome.actions.is_empty());
}
