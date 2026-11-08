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

fn workspace_row<'a>(
    state: &'a ClientShellState,
    endpoint_id: &ClientEndpointId,
    workspace_id: &str,
) -> &'a WorkspaceHit {
    state
        .hits
        .workspaces
        .iter()
        .find(|hit| &hit.endpoint_id == endpoint_id && hit.workspace_id == workspace_id)
        .expect("workspace hit")
}

fn workspace_marker(
    buffer: &Buffer,
    state: &ClientShellState,
    endpoint_id: &ClientEndpointId,
    workspace_id: &str,
) -> String {
    let rect = workspace_row(state, endpoint_id, workspace_id).rect;
    buffer[(rect.right() - 2, rect.y)].symbol().to_owned()
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

#[test]
fn pinned_workspace_row_shows_a_pin_marker() {
    let mut state = state_with_workspaces(&["ws_1", "ws_2"]);
    state.set_pane_surface(surface());
    state.pinned_workspaces.insert("ws_1".into());
    let frame = state.compose(106, 20).expect("composed frame");
    let buffer = frame.to_ratatui_buffer().expect("sidebar buffer");

    assert_eq!(
        workspace_marker(&buffer, &state, &ClientEndpointId::Local, "ws_1"),
        crate::client::shell::sidebar::PINNED_WORKSPACE_MARKER
    );
    assert_ne!(
        workspace_marker(&buffer, &state, &ClientEndpointId::Local, "ws_2"),
        crate::client::shell::sidebar::PINNED_WORKSPACE_MARKER
    );
}

#[test]
fn narrow_long_worktree_group_keeps_marker_left_of_fold_arrow() {
    let mut projected = snapshot_with_workspaces(&["parent", "child"]);
    projected.workspaces[0].label = "a-very-long-workspace-name-that-must-be-clipped".into();
    projected.workspaces[0].worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: false,
    });
    projected.workspaces[1].worktree = Some(ClientShellWorktree {
        key: "repo".into(),
        label: "repo".into(),
        is_linked_worktree: true,
    });
    let mut state = ClientShellState::new(ClientShellConfig::from_config(&Config::default()));
    state.sidebar_width = state.config.sidebar_min_width;
    state.set_snapshot(Box::new(projected));
    state.set_pane_surface(surface());
    state.pinned_workspaces = HashSet::from(["parent".into(), "child".into()]);

    let frame = state.compose(106, 20).expect("narrow sidebar frame");
    let buffer = frame.to_ratatui_buffer().expect("sidebar buffer");
    let parent = workspace_row(&state, &ClientEndpointId::Local, "parent");
    let (toggle, _) = parent.group_toggle.as_ref().expect("worktree fold arrow");
    assert_eq!(
        buffer[(parent.rect.right() - 2, parent.rect.y)].symbol(),
        crate::client::shell::sidebar::PINNED_WORKSPACE_MARKER
    );
    assert_eq!(toggle.x, parent.rect.right() - 1);
    assert_eq!(buffer[(toggle.x, toggle.y)].symbol(), "▾");
    assert_ne!(
        workspace_marker(&buffer, &state, &ClientEndpointId::Local, "child"),
        crate::client::shell::sidebar::PINNED_WORKSPACE_MARKER
    );
}

#[test]
fn remote_pinned_workspace_row_shows_a_pin_marker() {
    let mut state = state_with_workspaces(&["local"]);
    state.set_pane_surface(surface());
    let profile =
        crate::client::endpoint::SavedSshEndpoint::new("Remote", "dev@example.invalid", "test")
            .expect("remote profile");
    let remote_id = ClientEndpointId::Ssh(profile.id.clone());
    state.set_endpoint_catalog(&[profile]);
    state.set_endpoint_status(
        &remote_id,
        crate::client::endpoint::ClientEndpointStatus::Online,
    );
    let mut remote = snapshot_with_workspaces(&["remote"]);
    remote.boot_id = "remote-boot".into();
    state.set_endpoint_snapshot(&remote_id, Box::new(remote));
    state
        .remote_pinned_workspaces
        .insert(remote_id.clone(), HashSet::from(["remote".into()]));

    let frame = state.compose(106, 20).expect("remote sidebar frame");
    let buffer = frame.to_ratatui_buffer().expect("sidebar buffer");
    assert_eq!(
        workspace_marker(&buffer, &state, &remote_id, "remote"),
        crate::client::shell::sidebar::PINNED_WORKSPACE_MARKER
    );
}

#[test]
fn unpin_removes_the_marker_before_the_move_snapshot_arrives() {
    let mut state = state_with_workspaces(&["ws_1", "ws_2"]);
    state.set_pane_surface(surface());
    state.pinned_workspaces.insert("ws_1".into());
    let pinned = state.compose(106, 20).expect("pinned frame");
    let pinned = pinned.to_ratatui_buffer().expect("pinned buffer");
    assert_eq!(
        workspace_marker(&pinned, &state, &ClientEndpointId::Local, "ws_1"),
        crate::client::shell::sidebar::PINNED_WORKSPACE_MARKER
    );

    state.toggle_workspace_pin("ws_1".into(), &mut ClientShellInput::default());
    let unpinned = state.compose(106, 20).expect("unpinned frame");
    let unpinned = unpinned.to_ratatui_buffer().expect("unpinned buffer");
    assert_ne!(
        workspace_marker(&unpinned, &state, &ClientEndpointId::Local, "ws_1"),
        crate::client::shell::sidebar::PINNED_WORKSPACE_MARKER
    );
}
