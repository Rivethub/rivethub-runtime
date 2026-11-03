use std::time::Duration;

use kernel::plugins::{
    ApprovalError, Permission, PluginAccess, access, approve, narrowed, pending, restart_delay,
    split_tool_name, tool_name,
};

#[test]
fn routes_need_the_permission_their_family_names() {
    assert_eq!(access(true, "/api/plugin/self"), PluginAccess::Own);
    assert_eq!(
        access(false, "/api/plugin/views/changed"),
        PluginAccess::Own
    );
    assert_eq!(
        access(false, "/api/plugin/notify"),
        PluginAccess::Needs(Permission::NotificationsSend)
    );
    assert_eq!(
        access(true, "/api/memory/search"),
        PluginAccess::Needs(Permission::MemoryRead)
    );
    assert_eq!(
        access(false, "/api/memory/tool/memory_search"),
        PluginAccess::Needs(Permission::MemoryRead)
    );
    assert_eq!(
        access(false, "/api/memory/tool/memory_append"),
        PluginAccess::Needs(Permission::MemoryWrite)
    );
    assert_eq!(
        access(false, "/api/capture"),
        PluginAccess::Needs(Permission::MemoryWrite)
    );
    assert_eq!(
        access(true, "/api/harness-sessions/abc/transcript"),
        PluginAccess::Needs(Permission::SessionsRead)
    );
    assert_eq!(
        access(false, "/api/harness-sessions"),
        PluginAccess::Needs(Permission::SessionsControl)
    );
    assert_eq!(
        access(false, "/api/topics/abc/close"),
        PluginAccess::Needs(Permission::TopicsWrite)
    );
    assert_eq!(
        access(false, "/api/sessions/abc/placement"),
        PluginAccess::Needs(Permission::TopicsWrite)
    );
    assert_eq!(
        access(true, "/api/sessions/history"),
        PluginAccess::Needs(Permission::TopicsRead)
    );
    assert_eq!(
        access(true, "/api/sessions/suggestions"),
        PluginAccess::Needs(Permission::TopicsRead)
    );
    assert_eq!(
        access(false, "/api/sessions/abc/messages"),
        PluginAccess::Needs(Permission::SessionsControl)
    );
    assert_eq!(
        access(false, "/api/tasks"),
        PluginAccess::Needs(Permission::TasksDelegate)
    );
    assert_eq!(
        access(true, "/api/tasks/abc"),
        PluginAccess::Needs(Permission::TasksRead)
    );
    for refused in [
        (true, "/api/plugins"),
        (false, "/api/plugins/files/enable"),
        (false, "/api/memory/tags"),
        (false, "/api/tasks/abc/kill"),
        (true, "/api/files/list"),
        (false, "/api/sessions/abc/anything"),
        (true, "/api/sessions/abc/messages"),
        (true, "/mesh.json"),
    ] {
        assert_eq!(
            access(refused.0, refused.1),
            PluginAccess::Refused,
            "{refused:?}"
        );
    }
}

#[test]
fn the_agent_catalog_needs_agents_read_and_its_old_root_is_gone() {
    assert_eq!(
        access(true, "/api/catalog/agents"),
        PluginAccess::Needs(Permission::AgentsRead)
    );
    assert_eq!(access(true, "/api/catalog"), PluginAccess::Refused);
}

#[test]
fn approval_names_exactly_the_pending_permissions() {
    let requested = [Permission::SessionsRead, Permission::TasksRead];
    assert_eq!(
        pending(&requested, &[Permission::SessionsRead]),
        [Permission::TasksRead]
    );
    assert_eq!(
        approve(&requested, &[], &[Permission::SessionsRead]),
        Err(ApprovalError::Unapproved(vec![Permission::TasksRead]))
    );
    assert_eq!(
        approve(
            &requested,
            &[],
            &[
                Permission::SessionsRead,
                Permission::TasksRead,
                Permission::HostExec
            ]
        ),
        Err(ApprovalError::NotPending(vec![Permission::HostExec]))
    );
    assert_eq!(
        approve(
            &requested,
            &[Permission::SessionsRead],
            &[Permission::TasksRead]
        ),
        Ok(requested.to_vec())
    );
    assert_eq!(
        narrowed(&[Permission::TasksRead], &requested),
        [Permission::TasksRead]
    );
}

#[test]
fn tool_names_join_and_split_on_the_last_hyphen() {
    let (plugin, tool) = split_tool_name("daily-notes-start_run").unwrap();
    assert_eq!(plugin.as_ref(), "daily-notes");
    assert_eq!(tool.as_ref(), "start_run");
    assert_eq!(tool_name(&plugin, &tool), "daily-notes-start_run");
    assert!(split_tool_name("memory_search").is_err());
    assert!(split_tool_name("workflows__start_run").is_err());
    assert!(split_tool_name("Bad-x").is_err());
}

#[test]
fn restarts_back_off_from_one_second_to_a_minute() {
    assert_eq!(restart_delay(1), Duration::from_secs(1));
    assert_eq!(restart_delay(2), Duration::from_secs(2));
    assert_eq!(restart_delay(6), Duration::from_secs(32));
    assert_eq!(restart_delay(7), Duration::from_secs(60));
    assert_eq!(restart_delay(40), Duration::from_secs(60));
}
