use super::model::{Channel, Guild};

pub const CREATE_INSTANT_INVITE: u64 = 1 << 0;
const ADMINISTRATOR: u64 = 1 << 3;
pub const MANAGE_CHANNELS: u64 = 1 << 4;
const VIEW_CHANNEL: u64 = 1 << 10;
pub const SEND_MESSAGES: u64 = 1 << 11;
pub const ATTACH_FILES: u64 = 1 << 15;
pub const MANAGE_THREADS: u64 = 1 << 34;
pub const SEND_MESSAGES_IN_THREADS: u64 = 1 << 38;
pub const MANAGE_MESSAGES: u64 = 1 << 13;
pub const CONNECT: u64 = 1 << 20;
pub const CHANGE_NICKNAME: u64 = 1 << 26;

/// Our server-wide permissions (roles only, no channel overwrites).
pub fn guild_permissions(guild: &Guild, my_id: &str) -> Option<u64> {
    if guild.owner_id.as_deref()? == my_id {
        return Some(u64::MAX);
    }
    let my_roles = guild.my_roles.as_ref()?;
    let permissions = guild
        .roles
        .iter()
        .filter(|r| r.id == guild.id || my_roles.contains(&r.id))
        .fold(0, |all, role| all | role.permissions);
    Some(if permissions & ADMINISTRATOR != 0 { u64::MAX } else { permissions })
}

/// Our effective permissions in `channel`, or `None` when the owner, roles or
/// membership needed to compute them were not received.
pub fn channel_permissions(guild: &Guild, channel: &Channel, my_id: &str) -> Option<u64> {
    // Threads take their parent channel's permissions.
    if channel.is_thread() {
        let parent = guild.channels.iter().find(|c| Some(&c.id) == channel.parent_id.as_ref())?;
        return channel_permissions(guild, parent, my_id);
    }
    if guild.owner_id.as_deref()? == my_id {
        return Some(u64::MAX);
    }
    let my_roles = guild.my_roles.as_ref()?;
    let everyone = guild.roles.iter().find(|r| r.id == guild.id)?;

    let mut permissions = everyone.permissions;
    for role in guild.roles.iter().filter(|r| my_roles.contains(&r.id)) {
        permissions |= role.permissions;
    }
    if permissions & ADMINISTRATOR != 0 {
        return Some(u64::MAX);
    }

    let overwrites = &channel.permission_overwrites;
    if let Some(everyone) = overwrites.iter().find(|o| o.id == guild.id) {
        permissions = (permissions & !everyone.deny) | everyone.allow;
    }
    let (mut allow, mut deny) = (0, 0);
    for overwrite in overwrites.iter().filter(|o| o.kind == 0 && my_roles.contains(&o.id)) {
        allow |= overwrite.allow;
        deny |= overwrite.deny;
    }
    permissions = (permissions & !deny) | allow;
    if let Some(member) = overwrites.iter().find(|o| o.kind == 1 && o.id == my_id) {
        permissions = (permissions & !member.deny) | member.allow;
    }
    Some(permissions)
}

/// Removes text channels we definitely cannot view. Unknown permissions keep the channel.
pub fn hide_unviewable(guild: &mut Guild, my_id: &str) {
    let hidden: Vec<String> = guild
        .channels
        .iter()
        .filter(|c| !c.is_category())
        .filter(|c| channel_permissions(guild, c, my_id).is_some_and(|p| p & VIEW_CHANNEL == 0))
        .map(|c| c.id.clone())
        .collect();
    // Threads go with a hidden parent (checked above via the parent, too).
    guild.channels.retain(|c| !hidden.contains(&c.id) && !c.parent_id.as_ref().is_some_and(|p| c.is_thread() && hidden.contains(p)));
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;
    use crate::discord::model::RawGuild;

    const ME: &str = "9";

    /// Server "1" (so @everyone is role "1"), owned by someone else; we hold
    /// role "2". @everyone may view and send unless a channel says otherwise.
    fn guild(channels: Value) -> Guild {
        let raw: RawGuild = serde_json::from_value(json!({
            "id": "1",
            "owner_id": "8",
            "roles": [
                { "id": "1", "permissions": (VIEW_CHANNEL | SEND_MESSAGES).to_string() },
                { "id": "2", "permissions": "0" },
            ],
            "members": [{ "user_id": ME, "roles": ["2"] }],
            "channels": channels,
        }))
        .unwrap();
        let mut guild = Guild::from(raw);
        guild.finish(ME);
        guild
    }

    fn overwrite(id: &str, kind: u8, allow: u64, deny: u64) -> Value {
        json!({ "id": id, "type": kind, "allow": allow.to_string(), "deny": deny.to_string() })
    }

    #[test]
    fn overwrites_apply_everyone_then_roles_then_member() {
        let guild = guild(json!([
            // @everyone can't see it, but our role can.
            { "id": "10", "type": 0, "permission_overwrites": [overwrite("1", 0, 0, VIEW_CHANNEL), overwrite("2", 0, VIEW_CHANNEL, 0)] },
            // Only roles we don't have can see it.
            { "id": "11", "type": 0, "permission_overwrites": [overwrite("1", 0, 0, VIEW_CHANNEL)] },
            // Our role may send, but we as a member may not.
            { "id": "12", "type": 0, "permission_overwrites": [overwrite("2", 0, SEND_MESSAGES, 0), overwrite(ME, 1, 0, SEND_MESSAGES)] },
            { "id": "13", "type": 11, "parent_id": "12" },
        ]));
        let ids: Vec<&str> = guild.channels.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids, ["10", "12", "13"], "only the channel our roles can't view is hidden");
        let granted = |id: &str| channel_permissions(&guild, guild.channels.iter().find(|c| c.id == id).unwrap(), ME);
        assert!(granted("10").is_some_and(|p| p & VIEW_CHANNEL != 0), "a role allow beats an @everyone deny");
        assert!(granted("12").is_some_and(|p| p & SEND_MESSAGES == 0), "a member deny beats a role allow");
        assert_eq!(granted("13"), granted("12"), "threads take their parent's permissions");

        let mut owned = guild.clone();
        owned.owner_id = Some(ME.to_owned());
        assert_eq!(channel_permissions(&owned, &owned.channels[1], ME), Some(u64::MAX), "the owner may do anything");
        let mut unknown = guild.clone();
        unknown.my_roles = None;
        assert_eq!(channel_permissions(&unknown, &unknown.channels[1], ME), None, "no membership: unknown, so nothing is locked");
    }
}
