use kernel::plugins::{ApprovalError, EventKind, Permission};
use wire::plugin::manifest::{Permission as PermissionBody, PluginEventKind, PluginManifest};
use wire::plugin::process::PluginEvent;

pub(super) fn pending(
    requested: &[PermissionBody],
    granted: &[PermissionBody],
) -> Vec<PermissionBody> {
    bodies(&kernel::plugins::pending(
        &permissions(requested),
        &permissions(granted),
    ))
}

pub(super) fn narrowed(
    requested: &[PermissionBody],
    granted: &[PermissionBody],
) -> Vec<PermissionBody> {
    bodies(&kernel::plugins::narrowed(
        &permissions(requested),
        &permissions(granted),
    ))
}

pub(super) fn approve(
    requested: &[PermissionBody],
    granted: &[PermissionBody],
    approved: &[PermissionBody],
) -> Result<Vec<PermissionBody>, ApprovalError> {
    kernel::plugins::approve(
        &permissions(requested),
        &permissions(granted),
        &permissions(approved),
    )
    .map(|granted| bodies(&granted))
}

pub(super) const fn event_permission(kind: PluginEventKind) -> PermissionBody {
    body(kernel::plugins::event_permission(event(kind)))
}

pub(super) const fn event_kind(event: &PluginEvent) -> Option<PluginEventKind> {
    match event {
        PluginEvent::SessionStarted { .. } => Some(PluginEventKind::SessionStarted),
        PluginEvent::SessionIdle { .. } => Some(PluginEventKind::SessionIdle),
        PluginEvent::SessionEnded { .. } => Some(PluginEventKind::SessionEnded),
        PluginEvent::SessionPlaced { .. } => Some(PluginEventKind::SessionPlaced),
        PluginEvent::SummaryWritten { .. } => Some(PluginEventKind::SummaryWritten),
        PluginEvent::SettingsChanged { .. } => None,
    }
}

pub(super) fn wants(manifest: &PluginManifest, event: &PluginEvent) -> bool {
    event_kind(event).is_none_or(|kind| manifest.events.contains(&kind))
}

pub(super) const fn body(permission: Permission) -> PermissionBody {
    match permission {
        Permission::MemoryRead => PermissionBody::MemoryRead,
        Permission::MemoryWrite => PermissionBody::MemoryWrite,
        Permission::SessionsRead => PermissionBody::SessionsRead,
        Permission::SessionsControl => PermissionBody::SessionsControl,
        Permission::TopicsRead => PermissionBody::TopicsRead,
        Permission::TopicsWrite => PermissionBody::TopicsWrite,
        Permission::NotificationsSend => PermissionBody::NotificationsSend,
        Permission::HostFilesRead => PermissionBody::HostFilesRead,
        Permission::HostFilesWrite => PermissionBody::HostFilesWrite,
        Permission::HostNetwork => PermissionBody::HostNetwork,
        Permission::HostExec => PermissionBody::HostExec,
    }
}

const fn permission(body: PermissionBody) -> Permission {
    match body {
        PermissionBody::MemoryRead => Permission::MemoryRead,
        PermissionBody::MemoryWrite => Permission::MemoryWrite,
        PermissionBody::SessionsRead => Permission::SessionsRead,
        PermissionBody::SessionsControl => Permission::SessionsControl,
        PermissionBody::TopicsRead => Permission::TopicsRead,
        PermissionBody::TopicsWrite => Permission::TopicsWrite,
        PermissionBody::NotificationsSend => Permission::NotificationsSend,
        PermissionBody::HostFilesRead => Permission::HostFilesRead,
        PermissionBody::HostFilesWrite => Permission::HostFilesWrite,
        PermissionBody::HostNetwork => Permission::HostNetwork,
        PermissionBody::HostExec => Permission::HostExec,
    }
}

const fn event(kind: PluginEventKind) -> EventKind {
    match kind {
        PluginEventKind::SessionStarted => EventKind::SessionStarted,
        PluginEventKind::SessionIdle => EventKind::SessionIdle,
        PluginEventKind::SessionEnded => EventKind::SessionEnded,
        PluginEventKind::SessionPlaced => EventKind::SessionPlaced,
        PluginEventKind::SummaryWritten => EventKind::SummaryWritten,
    }
}

fn permissions(bodies: &[PermissionBody]) -> Vec<Permission> {
    bodies.iter().copied().map(permission).collect()
}

fn bodies(permissions: &[Permission]) -> Vec<PermissionBody> {
    permissions.iter().copied().map(body).collect()
}
