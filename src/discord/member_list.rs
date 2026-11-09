//! The member sidebar for user sessions, kept in sync by GUILD_MEMBER_LIST_UPDATE.

use serde::Deserialize;

use super::model::User;

/// Rows kept per list (groups and members); matches the range we subscribe to.
pub const LIMIT: usize = 100;

#[derive(Debug, Clone, Deserialize)]
pub struct Group {
    /// "online", "offline", or a hoisted role id.
    pub id: String,
    #[serde(default)]
    pub count: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Presence {
    #[serde(default)]
    pub status: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Member {
    pub user: User,
    #[serde(default)]
    pub nick: Option<String>,
    #[serde(default)]
    pub presence: Option<Presence>,
}

impl Member {
    pub fn name(&self) -> &str {
        self.nick.as_deref().unwrap_or_else(|| self.user.display_name())
    }

    pub fn status(&self) -> &str {
        self.presence.as_ref().map_or("offline", |p| p.status.as_str())
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Item {
    Group(Group),
    Member(Member),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "op", rename_all = "UPPERCASE")]
pub enum Op {
    Sync { range: [usize; 2], items: Vec<Item> },
    Insert { index: usize, item: Item },
    Update { index: usize, item: Item },
    Delete { index: usize },
    Invalidate { range: [usize; 2] },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Update {
    pub guild_id: String,
    #[serde(default)]
    pub online_count: u32,
    #[serde(default)]
    pub member_count: u32,
    /// Every group with its member count. The group rows themselves often
    /// leave their count out, so the counts shown come from here.
    #[serde(default)]
    pub groups: Vec<Group>,
    #[serde(default)]
    pub ops: Vec<Op>,
}

#[derive(Debug, Default)]
pub struct MemberList {
    /// `None` rows were invalidated or not sent yet.
    pub rows: Vec<Option<Item>>,
    pub online: u32,
    pub total: u32,
    /// Members per group id, from the latest update.
    counts: std::collections::HashMap<String, u32>,
}

impl MemberList {
    /// How many members a group has (its row's own count if the list had none).
    pub fn count(&self, group: &Group) -> u32 {
        self.counts.get(&group.id).copied().unwrap_or(group.count)
    }

    pub fn apply(&mut self, update: Update) {
        self.online = update.online_count;
        self.total = update.member_count;
        if !update.groups.is_empty() {
            self.counts = update.groups.into_iter().map(|g| (g.id, g.count)).collect();
        }
        for op in update.ops {
            match op {
                Op::Sync { range: [start, _], items } => {
                    for (offset, item) in items.into_iter().enumerate() {
                        self.set(start + offset, item);
                    }
                }
                Op::Insert { index, item } if index <= self.rows.len() && index < LIMIT => {
                    self.rows.insert(index, Some(item));
                    self.rows.truncate(LIMIT);
                }
                Op::Insert { index, item } => self.set(index, item),
                Op::Update { index, item } => self.set(index, item),
                Op::Delete { index } if index < self.rows.len() => {
                    self.rows.remove(index);
                }
                Op::Invalidate { range: [start, end] } => {
                    for row in self.rows.iter_mut().take(end.saturating_add(1)).skip(start) {
                        *row = None;
                    }
                }
                Op::Delete { .. } | Op::Unknown => {}
            }
        }
    }

    fn set(&mut self, index: usize, item: Item) {
        if index >= LIMIT {
            return;
        }
        if self.rows.len() <= index {
            self.rows.resize(index + 1, None);
        }
        self.rows[index] = Some(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &MemberList) -> Vec<String> {
        list.rows
            .iter()
            .map(|row| match row {
                Some(Item::Group(g)) => format!("[{}]", g.id),
                Some(Item::Member(m)) => m.name().to_owned(),
                None => "-".to_owned(),
            })
            .collect()
    }

    #[test]
    fn applies_ops() {
        let update: Update = serde_json::from_value(serde_json::json!({
            "guild_id": "1", "online_count": 2, "member_count": 5,
            "groups": [{ "id": "online", "count": 2 }, { "id": "offline", "count": 3 }],
            "ops": [
                { "op": "SYNC", "range": [0, 99], "items": [
                    { "group": { "id": "online" } },
                    { "member": { "user": { "id": "10", "username": "a" }, "presence": { "status": "online" } } },
                    { "member": { "user": { "id": "11", "username": "b" }, "nick": "bee" } }
                ]},
                { "op": "INSERT", "index": 1, "item": { "member": { "user": { "id": "12", "username": "c" } } } },
                { "op": "DELETE", "index": 3 },
                { "op": "SOMETHING_NEW" }
            ]
        }))
        .unwrap();
        let mut list = MemberList::default();
        list.apply(update);
        assert_eq!(names(&list), ["[online]", "c", "a"]);
        assert_eq!((list.online, list.total), (2, 5));
        let Some(Item::Group(online)) = &list.rows[0] else { panic!("a group row") };
        assert_eq!(list.count(online), 2, "the count comes from `groups`, not the row");
    }
}
