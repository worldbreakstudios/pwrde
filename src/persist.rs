//! Session persistence — snapshots group layouts, sidebar sections, and
//! shpool sessions to SQLite.
//!
//! The default DB path is `<data_dir>/pwrde/state.db`. When the process is
//! launched from a linked git worktree (see [`crate::git::worktree_scope`]),
//! the path becomes `<data_dir>/pwrde/worktrees/<slug>/state.db` so each
//! worktree keeps an isolated session database. Primary checkouts and
//! non-git launches keep the unscoped default.
//!
//! Schema evolution: `open_db` creates tables with `CREATE TABLE IF NOT EXISTS`
//! and migrates older DBs by adding `groups.section_id` and `groups.pinned`
//! (duplicate-column errors are ignored) plus a `sections` table for collapsible
//! sidebar groups.

#[cfg(not(target_family = "wasm"))]
use rusqlite::{Connection, Result as SqlResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Serializable layout tree mirroring workspace::Node without live state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LayoutNode {
    Leaf {
        tile: usize,
        /// Pane collapse state; absent in pre-collapse snapshots.
        #[serde(default)]
        collapsed: bool,
    },
    Split { dir: String, ratio: f32, a: Box<LayoutNode>, b: Box<LayoutNode> },
}

/// A saved group's metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedGroup {
    pub position: usize,
    pub name: String,
    pub cwd: Option<String>,
    pub focused_tile: usize,
    pub layout: LayoutNode,
    pub tabs: Vec<SavedTab>,
    /// Sidebar section membership, if any.
    pub section_id: Option<u64>,
    /// Whether this group is pinned to the Sessions sidebar quick-access strip.
    pub pinned: bool,
}

/// A saved collapsible sidebar section.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedSection {
    pub id: u64,
    pub position: usize,
    pub name: String,
    pub emoji: String,
    pub collapsed: bool,
    pub anchor: Option<u64>,
}

/// A saved tab within a group.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedTab {
    pub tile_id: usize,
    pub tab_index: usize,
    pub active: bool,
    pub shpool_session: Option<String>,
    pub cwd: Option<String>,
    pub unread: bool,
    pub unread_at: Option<i64>,
}

/// Compose the DB path under `data_dir`, optionally scoped to a worktree slug.
///
/// - `None` scope → `<data_dir>/pwrde/state.db`
/// - `Some(slug)` → `<data_dir>/pwrde/worktrees/<slug>/state.db`
pub fn db_path_in(data_dir: &Path, scope: Option<&str>) -> PathBuf {
    let mut path = data_dir.join("pwrde");
    if let Some(slug) = scope {
        path = path.join("worktrees").join(slug);
    }
    path.join("state.db")
}

/// Location of the persisted DB: `<data_dir>/pwrde/state.db`, or
/// `<data_dir>/pwrde/worktrees/<slug>/state.db` when launched from a linked worktree.
#[cfg(not(target_family = "wasm"))]
fn db_path() -> Option<PathBuf> {
    let data_dir = dirs::data_dir()?;
    let scope = crate::git::worktree_scope();
    Some(db_path_in(&data_dir, scope.as_deref()))
}

/// Open the DB at `path`, creating tables if needed. Never panics on corrupt DB.
#[cfg(not(target_family = "wasm"))]
fn open_db(path: &Path) -> SqlResult<Connection> {
    // Create parent directory if missing
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }

    let conn = Connection::open(path)?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS groups (
            id INTEGER PRIMARY KEY,
            position INTEGER NOT NULL,
            name TEXT NOT NULL,
            cwd TEXT,
            focused_tile INTEGER NOT NULL,
            layout TEXT NOT NULL
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS tabs (
            id INTEGER PRIMARY KEY,
            group_id INTEGER NOT NULL,
            tile_id INTEGER NOT NULL,
            tab_index INTEGER NOT NULL,
            active INTEGER NOT NULL,
            shpool_session TEXT,
            cwd TEXT
        )",
        [],
    )?;

    // Migrate pre-sections DBs: CREATE TABLE IF NOT EXISTS does not add columns
    // to existing tables, so ALTER and ignore the duplicate-column error.
    let _ = conn.execute("ALTER TABLE groups ADD COLUMN section_id INTEGER", []);
    // Migrate pre-pin DBs; duplicate-column errors are intentionally ignored.
    let _ = conn.execute("ALTER TABLE groups ADD COLUMN pinned INTEGER", []);
    let _ = conn.execute("ALTER TABLE tabs ADD COLUMN unread INTEGER", []);
    // Migrate pre-attention DBs; duplicate-column errors are intentionally ignored.
    let _ = conn.execute("ALTER TABLE tabs ADD COLUMN unread_at INTEGER", []);

    conn.execute(
        "CREATE TABLE IF NOT EXISTS sections (
            id INTEGER PRIMARY KEY,
            position INTEGER NOT NULL,
            name TEXT NOT NULL,
            emoji TEXT NOT NULL,
            collapsed INTEGER NOT NULL,
            anchor_tile INTEGER
        )",
        [],
    )?;
    let _ = conn.execute("ALTER TABLE sections ADD COLUMN anchor_tile INTEGER", []);

    Ok(conn)
}

/// Save a snapshot to the DB, rewriting all groups and sections in one transaction.
/// Returns Ok(()) on success, Err on DB failure. Never panics.
#[cfg(not(target_family = "wasm"))]
pub fn save_snapshot(
    groups: &[SavedGroup],
    sections: &[SavedSection],
    path: &Path,
) -> SqlResult<()> {
    let conn = open_db(path)?;
    let tx = conn.unchecked_transaction()?;

    tx.execute("DELETE FROM tabs", [])?;
    tx.execute("DELETE FROM groups", [])?;
    tx.execute("DELETE FROM sections", [])?;

    for section in sections {
        tx.execute(
            "INSERT INTO sections (id, position, name, emoji, collapsed, anchor_tile)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            (
                section.id as i64,
                section.position,
                &section.name,
                &section.emoji,
                if section.collapsed { 1 } else { 0 },
                section.anchor.map(|t| t as i64),
            ),
        )?;
    }

    for group in groups {
        let layout_json = serde_json::to_string(&group.layout).unwrap_or_else(|e| {
            eprintln!("persist: failed to serialize layout: {}", e);
            String::from("{}")
        });

        tx.execute(
            "INSERT INTO groups (position, name, cwd, focused_tile, layout, section_id, pinned)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            (
                group.position,
                &group.name,
                &group.cwd,
                group.focused_tile,
                &layout_json,
                group.section_id.map(|id| id as i64),
                if group.pinned { 1 } else { 0 },
            ),
        )?;

        let group_id = tx.last_insert_rowid();

        for tab in &group.tabs {
            tx.execute(
                "INSERT INTO tabs (group_id, tile_id, tab_index, active, shpool_session, cwd, unread, unread_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                (
                    group_id,
                    tab.tile_id,
                    tab.tab_index,
                    if tab.active { 1 } else { 0 },
                    &tab.shpool_session,
                    &tab.cwd,
                    if tab.unread { 1 } else { 0 },
                    tab.unread_at,
                ),
            )?;
        }
    }

    tx.commit()?;
    Ok(())
}

/// Load the snapshot from the DB, returning saved groups and sections.
/// Returns empty vectors on missing/corrupt DB. Never panics.
#[cfg(not(target_family = "wasm"))]
pub fn load_snapshot(path: &Path) -> (Vec<SavedGroup>, Vec<SavedSection>) {
    let conn = match open_db(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("persist: failed to open DB: {}", e);
            return (Vec::new(), Vec::new());
        }
    };

    let sections = load_sections(&conn);
    let groups = load_groups(&conn);
    (groups, sections)
}

#[cfg(not(target_family = "wasm"))]
fn load_sections(conn: &Connection) -> Vec<SavedSection> {
    let mut stmt = match conn.prepare(
        "SELECT id, position, name, emoji, collapsed, anchor_tile FROM sections ORDER BY position",
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("persist: failed to prepare sections query: {}", e);
            return Vec::new();
        }
    };

    let rows = match stmt.query_map([], |row| {
        Ok(SavedSection {
            id: row.get::<_, i64>(0)? as u64,
            position: row.get(1)?,
            name: row.get(2)?,
            emoji: row.get(3)?,
            collapsed: row.get::<_, i32>(4)? != 0,
            anchor: row.get::<_, Option<i64>>(5)?.map(|v| v as u64),
        })
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("persist: failed to query sections: {}", e);
            return Vec::new();
        }
    };

    let mut sections = Vec::new();
    for row_result in rows {
        match row_result {
            Ok(s) => sections.push(s),
            Err(e) => eprintln!("persist: failed to read section row: {}", e),
        }
    }
    sections
}

#[cfg(not(target_family = "wasm"))]
fn load_groups(conn: &Connection) -> Vec<SavedGroup> {
    let mut stmt = match conn.prepare(
        "SELECT id, position, name, cwd, focused_tile, layout, section_id, pinned
         FROM groups ORDER BY position",
    ) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("persist: failed to prepare groups query: {}", e);
            return Vec::new();
        }
    };

    let rows = match stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, usize>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, usize>(4)?,
            row.get::<_, String>(5)?,
            row.get::<_, Option<i64>>(6)?,
            row.get::<_, Option<i64>>(7)?,
        ))
    }) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("persist: failed to query groups: {}", e);
            return Vec::new();
        }
    };

    let mut groups = Vec::new();

    for row_result in rows {
        let (group_id, position, name, cwd, focused_tile, layout_json, section_id, pinned) =
            match row_result {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("persist: failed to read group row: {}", e);
                    continue;
                }
            };

        let layout = match serde_json::from_str::<LayoutNode>(&layout_json) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("persist: failed to parse layout JSON: {}", e);
                continue;
            }
        };

        let mut tab_stmt = match conn.prepare(
            "SELECT tile_id, tab_index, active, shpool_session, cwd, unread, unread_at
             FROM tabs WHERE group_id = ?1 ORDER BY tile_id, tab_index",
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("persist: failed to prepare tabs query: {}", e);
                continue;
            }
        };

        let tab_rows = match tab_stmt.query_map([group_id], |row| {
            Ok(SavedTab {
                tile_id: row.get(0)?,
                tab_index: row.get(1)?,
                active: row.get::<_, i32>(2)? != 0,
                shpool_session: row.get(3)?,
                cwd: row.get(4)?,
                unread: row.get::<_, Option<i64>>(5)?.map(|v| v != 0).unwrap_or(false),
                unread_at: row.get::<_, Option<i64>>(6)?,
            })
        }) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("persist: failed to query tabs for group {}: {}", group_id, e);
                continue;
            }
        };

        let mut tabs = Vec::new();
        for tab_result in tab_rows {
            match tab_result {
                Ok(t) => tabs.push(t),
                Err(e) => eprintln!("persist: failed to read tab row: {}", e),
            }
        }

        groups.push(SavedGroup {
            position,
            name,
            cwd,
            focused_tile,
            layout,
            tabs,
            section_id: section_id.map(|id| id as u64),
            // NULL (pre-pin rows) and 0 both mean unpinned.
            pinned: pinned.map(|v| v != 0).unwrap_or(false),
        });
    }

    groups
}

/// Convert a system time to non-negative Unix epoch seconds.
pub fn to_epoch_secs(time: Option<SystemTime>) -> Option<i64> {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_secs()).ok())
}

/// Convert non-negative Unix epoch seconds to a system time.
pub fn from_epoch_secs(secs: Option<i64>) -> Option<SystemTime> {
    secs.filter(|&s| s >= 0)
        .and_then(|s| UNIX_EPOCH.checked_add(Duration::from_secs(s as u64)))
}

/// Convert live Workspace instances to SavedGroup format.
pub fn workspaces_to_saved(workspaces: &[crate::workspace::Workspace]) -> Vec<SavedGroup> {
    workspaces
        .iter()
        .enumerate()
        .map(|(position, ws)| {
            let (layout, tabs) = node_to_layout(&ws.root);
            SavedGroup {
                position,
                name: ws.name.clone(),
                cwd: ws.cwd.as_ref().map(|p| p.display().to_string()),
                focused_tile: ws.focused_tile as usize,
                layout,
                tabs,
                section_id: ws.section,
                pinned: ws.pinned,
            }
        })
        .collect()
}

/// Convert live sidebar sections to SavedSection format.
pub fn sections_to_saved(sections: &[crate::workspace::Section]) -> Vec<SavedSection> {
    sections
        .iter()
        .enumerate()
        .map(|(position, s)| SavedSection {
            id: s.id,
            position,
            name: s.name.clone(),
            emoji: s.emoji.clone(),
            collapsed: s.collapsed,
            anchor: s.anchor,
        })
        .collect()
}

/// Rebuild live sidebar sections from a saved snapshot, ordered by position.
pub fn saved_to_sections(saved: &[SavedSection]) -> Vec<crate::workspace::Section> {
    saved
        .iter()
        .map(|s| crate::workspace::Section {
            id: s.id,
            name: s.name.clone(),
            emoji: s.emoji.clone(),
            collapsed: s.collapsed,
            anchor: s.anchor,
        })
        .collect()
}

fn node_to_layout(node: &crate::workspace::Node) -> (LayoutNode, Vec<SavedTab>) {
    let mut tabs = Vec::new();
    let layout = node_to_layout_rec(node, &mut tabs);
    (layout, tabs)
}

fn node_to_layout_rec(node: &crate::workspace::Node, tabs: &mut Vec<SavedTab>) -> LayoutNode {
    use crate::workspace::Node;
    match node {
        Node::Leaf(tile) => {
            for (tab_index, tab) in tile.tabs.iter().enumerate() {
                tabs.push(SavedTab {
                    tile_id: tile.id as usize,
                    tab_index,
                    active: tab_index == tile.active,
                    shpool_session: tab.session.shpool_session.clone(),
                    cwd: None, // cwd is not tracked on Session; shpool will preserve it
                    unread: tab.unread,
                    unread_at: to_epoch_secs(tab.unread_at),
                });
            }
            LayoutNode::Leaf {
                tile: tile.id as usize,
                collapsed: tile.collapsed,
            }
        }
        Node::Split { dir, ratio, a, b } => {
            let dir_str = match dir {
                crate::workspace::Dir::Row => "row",
                crate::workspace::Dir::Column => "column",
            };
            LayoutNode::Split {
                dir: dir_str.to_string(),
                ratio: *ratio,
                a: Box::new(node_to_layout_rec(a, tabs)),
                b: Box::new(node_to_layout_rec(b, tabs)),
            }
        }
    }
}

/// Public API using the default DB path.
#[cfg(not(target_family = "wasm"))]
pub fn save_snapshot_default(
    groups: &[SavedGroup],
    sections: &[SavedSection],
) -> SqlResult<()> {
    match db_path() {
        Some(path) => save_snapshot(groups, sections, &path),
        None => {
            eprintln!("persist: cannot determine data directory");
            Ok(())
        }
    }
}

/// wasm32 has no SQLite (and no data dir): nothing is persisted, and a fresh
/// page always starts empty.
#[cfg(target_family = "wasm")]
pub fn save_snapshot_default(
    _groups: &[SavedGroup],
    _sections: &[SavedSection],
) -> Result<(), String> {
    Ok(())
}

/// Public API using the default DB path.
#[cfg(not(target_family = "wasm"))]
pub fn load_snapshot_default() -> (Vec<SavedGroup>, Vec<SavedSection>) {
    match db_path() {
        Some(path) => load_snapshot(&path),
        None => {
            eprintln!("persist: cannot determine data directory");
            (Vec::new(), Vec::new())
        }
    }
}

/// See [`save_snapshot_default`]: no persistence on wasm32.
#[cfg(target_family = "wasm")]
pub fn load_snapshot_default() -> (Vec<SavedGroup>, Vec<SavedSection>) {
    (Vec::new(), Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("pwrde-persist-test-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("state.db")
    }

    fn sample_group(position: usize, name: &str, section_id: Option<u64>) -> SavedGroup {
        SavedGroup {
            position,
            name: name.into(),
            cwd: Some("/home/user".into()),
            focused_tile: 1,
            layout: LayoutNode::Leaf { tile: 1, collapsed: false },
            tabs: vec![SavedTab {
                tile_id: 1,
                tab_index: 0,
                active: true,
                shpool_session: Some("pwrde-1-abc".into()),
                cwd: Some("/home/user".into()),
                unread: false,
                unread_at: None,
            }],
            section_id,
            pinned: false,
        }
    }

    #[test]
    fn collapsed_survives_roundtrip_and_legacy_layouts_load_expanded() {
        // A pre-collapse snapshot has no `collapsed` key at all.
        let legacy: LayoutNode = serde_json::from_str(r#"{"tile":5}"#).unwrap();
        assert_eq!(legacy, LayoutNode::Leaf { tile: 5, collapsed: false });

        let path = temp_db("collapsed");
        let mut group = sample_group(0, "main", None);
        group.layout = LayoutNode::Split {
            dir: "column".into(),
            ratio: 0.5,
            a: Box::new(LayoutNode::Leaf { tile: 1, collapsed: false }),
            b: Box::new(LayoutNode::Leaf { tile: 2, collapsed: true }),
        };
        save_snapshot(&[group], &[], &path).unwrap();
        let (loaded, _) = load_snapshot(&path);
        match &loaded[0].layout {
            LayoutNode::Split { a, b, .. } => {
                assert_eq!(**a, LayoutNode::Leaf { tile: 1, collapsed: false });
                assert_eq!(**b, LayoutNode::Leaf { tile: 2, collapsed: true });
            }
            _ => panic!("expected split layout"),
        }
    }

    #[test]
    fn roundtrip_two_groups_with_split() {
        let path = temp_db("roundtrip");

        let groups = vec![
            SavedGroup {
                position: 0,
                name: "main".into(),
                cwd: Some("/home/user".into()),
                focused_tile: 1,
                layout: LayoutNode::Split {
                    dir: "row".into(),
                    ratio: 0.3,
                    a: Box::new(LayoutNode::Leaf { tile: 1, collapsed: false }),
                    b: Box::new(LayoutNode::Leaf { tile: 2, collapsed: false }),
                },
                tabs: vec![
                    SavedTab {
                        tile_id: 1,
                        tab_index: 0,
                        active: true,
                        shpool_session: Some("pwrde-1-abc".into()),
                        cwd: Some("/home/user".into()),
                        unread: false,
                        unread_at: None,
                    },
                    SavedTab {
                        tile_id: 1,
                        tab_index: 1,
                        active: false,
                        shpool_session: Some("pwrde-2-def".into()),
                        cwd: Some("/tmp".into()),
                        unread: false,
                        unread_at: None,
                    },
                    SavedTab {
                        tile_id: 2,
                        tab_index: 0,
                        active: true,
                        shpool_session: None,
                        cwd: None,
                        unread: false,
                        unread_at: None,
                    },
                ],
                section_id: None,
                pinned: false,
            },
            SavedGroup {
                position: 1,
                name: "scratch".into(),
                cwd: None,
                focused_tile: 3,
                layout: LayoutNode::Leaf { tile: 3, collapsed: false },
                tabs: vec![SavedTab {
                    tile_id: 3,
                    tab_index: 0,
                    active: true,
                    shpool_session: Some("pwrde-3-xyz".into()),
                    cwd: Some("/var/log".into()),
                    unread: false,
                    unread_at: None,
                }],
                section_id: None,
                pinned: false,
            },
        ];

        save_snapshot(&groups, &[], &path).unwrap();
        let (loaded, sections) = load_snapshot(&path);

        assert_eq!(loaded.len(), 2);
        assert!(sections.is_empty());
        assert_eq!(loaded[0].name, "main");
        assert_eq!(loaded[0].position, 0);
        assert_eq!(loaded[0].focused_tile, 1);
        assert_eq!(loaded[0].cwd, Some("/home/user".into()));
        assert_eq!(loaded[0].section_id, None);

        // Verify split structure
        match &loaded[0].layout {
            LayoutNode::Split { dir, ratio, a, b } => {
                assert_eq!(dir, "row");
                assert!((ratio - 0.3).abs() < 0.001);
                assert_eq!(**a, LayoutNode::Leaf { tile: 1, collapsed: false });
                assert_eq!(**b, LayoutNode::Leaf { tile: 2, collapsed: false });
            }
            _ => panic!("expected split layout"),
        }

        // Verify tabs
        assert_eq!(loaded[0].tabs.len(), 3);
        assert_eq!(loaded[0].tabs[0].tile_id, 1);
        assert_eq!(loaded[0].tabs[0].tab_index, 0);
        assert_eq!(loaded[0].tabs[0].active, true);
        assert_eq!(
            loaded[0].tabs[0].shpool_session,
            Some("pwrde-1-abc".into())
        );
        assert_eq!(loaded[0].tabs[1].tile_id, 1);
        assert_eq!(loaded[0].tabs[1].tab_index, 1);
        assert_eq!(
            loaded[0].tabs[1].shpool_session,
            Some("pwrde-2-def".into())
        );

        assert_eq!(loaded[1].name, "scratch");
        assert_eq!(loaded[1].position, 1);
        assert_eq!(loaded[1].tabs.len(), 1);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn roundtrip_sections_membership_emoji_collapsed() {
        let path = temp_db("sections-roundtrip");

        let sections = vec![
            SavedSection {
                id: 10,
                position: 0,
                name: "work".into(),
                emoji: "👩‍💻".into(),
                collapsed: true,
                anchor: None,
            },
            SavedSection {
                id: 20,
                position: 1,
                name: "play".into(),
                emoji: String::new(),
                collapsed: false,
                anchor: None,
            },
        ];
        let groups = vec![
            sample_group(0, "a", Some(10)),
            sample_group(1, "b", Some(10)),
            sample_group(2, "c", None),
            sample_group(3, "d", Some(20)),
        ];

        save_snapshot(&groups, &sections, &path).unwrap();
        let (loaded_groups, loaded_sections) = load_snapshot(&path);

        assert_eq!(loaded_sections.len(), 2);
        assert_eq!(loaded_sections[0].id, 10);
        assert_eq!(loaded_sections[0].name, "work");
        assert_eq!(loaded_sections[0].emoji, "👩‍💻");
        assert!(loaded_sections[0].collapsed);
        assert_eq!(loaded_sections[1].id, 20);
        assert_eq!(loaded_sections[1].name, "play");
        assert_eq!(loaded_sections[1].emoji, "");
        assert!(!loaded_sections[1].collapsed);

        assert_eq!(loaded_groups.len(), 4);
        assert_eq!(loaded_groups[0].section_id, Some(10));
        assert_eq!(loaded_groups[1].section_id, Some(10));
        assert_eq!(loaded_groups[2].section_id, None);
        assert_eq!(loaded_groups[3].section_id, Some(20));

        // Live conversion helpers preserve fields.
        let live = saved_to_sections(&loaded_sections);
        assert_eq!(live[0].id, 10);
        assert_eq!(live[0].emoji, "👩‍💻");
        assert!(live[0].collapsed);
        let back = sections_to_saved(&live);
        assert_eq!(back[0].id, 10);
        assert_eq!(back[0].position, 0);

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn load_pre_sections_db_still_works() {
        let path = temp_db("pre-sections");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }

        // Hand-build a DB with the pre-sections schema (no section_id column,
        // no sections table) and one group row.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "CREATE TABLE groups (
                    id INTEGER PRIMARY KEY,
                    position INTEGER NOT NULL,
                    name TEXT NOT NULL,
                    cwd TEXT,
                    focused_tile INTEGER NOT NULL,
                    layout TEXT NOT NULL
                )",
                [],
            )
            .unwrap();
            conn.execute(
                "CREATE TABLE tabs (
                    id INTEGER PRIMARY KEY,
                    group_id INTEGER NOT NULL,
                    tile_id INTEGER NOT NULL,
                    tab_index INTEGER NOT NULL,
                    active INTEGER NOT NULL,
                    shpool_session TEXT,
                    cwd TEXT
                )",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO groups (position, name, cwd, focused_tile, layout)
                 VALUES (0, 'legacy', NULL, 1, ?1)",
                [r#"{"tile":1}"#],
            )
            .unwrap();
            let gid = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO tabs (group_id, tile_id, tab_index, active, shpool_session, cwd)
                 VALUES (?1, 1, 0, 1, 'old-sess', NULL)",
                [gid],
            )
            .unwrap();
        }

        let (groups, sections) = load_snapshot(&path);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].name, "legacy");
        assert_eq!(groups[0].section_id, None);
        assert!(!groups[0].pinned, "pre-pin rows load as pinned=false");
        assert_eq!(groups[0].tabs.len(), 1);
        assert_eq!(
            groups[0].tabs[0].shpool_session,
            Some("old-sess".into())
        );
        assert!(sections.is_empty());

        // Saving back through the new schema should succeed and keep membership None.
        save_snapshot(&groups, &sections, &path).unwrap();
        let (groups2, sections2) = load_snapshot(&path);
        assert_eq!(groups2.len(), 1);
        assert_eq!(groups2[0].section_id, None);
        assert!(!groups2[0].pinned);
        assert!(sections2.is_empty());

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn roundtrip_pinned_flag() {
        let path = temp_db("pinned-roundtrip");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }

        let mut pinned = sample_group(0, "pinned", None);
        pinned.pinned = true;
        let unpinned = sample_group(1, "loose", None);
        save_snapshot(&[pinned, unpinned], &[], &path).unwrap();

        let (loaded, _) = load_snapshot(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "pinned");
        assert!(loaded[0].pinned, "pinned group must survive the roundtrip");
        assert_eq!(loaded[1].name, "loose");
        assert!(!loaded[1].pinned, "unpinned group stays unpinned");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn missing_db_loads_empty() {
        let (groups, sections) = load_snapshot(Path::new("/nonexistent/pwrde/state.db"));
        assert_eq!(groups.len(), 0);
        assert_eq!(sections.len(), 0);
    }

    #[test]
    fn db_path_in_unscoped() {
        let base = Path::new("/data");
        assert_eq!(
            db_path_in(base, None),
            PathBuf::from("/data/pwrde/state.db")
        );
    }

    #[test]
    fn db_path_in_scoped() {
        let base = Path::new("/data");
        assert_eq!(
            db_path_in(base, Some("feature-branch")),
            PathBuf::from("/data/pwrde/worktrees/feature-branch/state.db")
        );
    }

    #[test]
    fn workspaces_to_saved_carries_section_id() {
        use crate::workspace::{Tile, Workspace};
        let mut a = Workspace::new("a".into(), Tile::empty(1), None);
        a.section = Some(7);
        let b = Workspace::new("b".into(), Tile::empty(2), None);
        let saved = workspaces_to_saved(&[a, b]);
        assert_eq!(saved[0].section_id, Some(7));
        assert_eq!(saved[1].section_id, None);
    }

    #[test]
    fn unread_survives_roundtrip() {
        let path = temp_db("unread-roundtrip");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }

        let groups = vec![SavedGroup {
            position: 0,
            name: "g".into(),
            cwd: None,
            focused_tile: 1,
            layout: LayoutNode::Leaf { tile: 1, collapsed: false },
            tabs: vec![
                SavedTab {
                    tile_id: 1,
                    tab_index: 0,
                    active: true,
                    shpool_session: None,
                    cwd: None,
                    unread: true,
                    unread_at: Some(1_700_000_000),
                },
                SavedTab {
                    tile_id: 1,
                    tab_index: 1,
                    active: false,
                    shpool_session: None,
                    cwd: None,
                    unread: false,
                    unread_at: None,
                },
            ],
            section_id: None,
            pinned: false,
        }];

        save_snapshot(&groups, &[], &path).unwrap();
        let (loaded, _) = load_snapshot(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].tabs.len(), 2);
        assert!(loaded[0].tabs[0].unread, "first tab should be unread");
        assert!(!loaded[0].tabs[1].unread, "second tab should not be unread");
        assert_eq!(
            loaded[0].tabs[0].unread_at,
            Some(1_700_000_000),
            "the moment a tab was marked unread should survive the roundtrip"
        );
        assert_eq!(loaded[0].tabs[1].unread_at, None, "a tab that never signalled has no stamp");

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn epoch_conversion_roundtrips_and_rejects_pre_epoch() {
        let time = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        assert_eq!(from_epoch_secs(to_epoch_secs(Some(time))), Some(time));
        assert_eq!(to_epoch_secs(Some(UNIX_EPOCH - Duration::from_secs(1))), None);
        assert_eq!(from_epoch_secs(Some(-1)), None);
    }

    #[test]
    fn pre_unread_db_loads_false() {
        // DB without the unread column should load tabs with unread=false.
        let path = temp_db("pre-unread");
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }

        {
            let conn = Connection::open(&path).unwrap();
            conn.execute(
                "CREATE TABLE groups (
                    id INTEGER PRIMARY KEY,
                    position INTEGER NOT NULL,
                    name TEXT NOT NULL,
                    cwd TEXT,
                    focused_tile INTEGER NOT NULL,
                    layout TEXT NOT NULL,
                    section_id INTEGER
                )",
                [],
            )
            .unwrap();
            conn.execute(
                "CREATE TABLE tabs (
                    id INTEGER PRIMARY KEY,
                    group_id INTEGER NOT NULL,
                    tile_id INTEGER NOT NULL,
                    tab_index INTEGER NOT NULL,
                    active INTEGER NOT NULL,
                    shpool_session TEXT,
                    cwd TEXT
                )",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO groups (position, name, cwd, focused_tile, layout)
                 VALUES (0, 'old', NULL, 1, ?1)",
                [r#"{"tile":1}"#],
            )
            .unwrap();
            let gid = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO tabs (group_id, tile_id, tab_index, active, shpool_session, cwd)
                 VALUES (?1, 1, 0, 1, NULL, NULL)",
                [gid],
            )
            .unwrap();
        }

        let (groups, _) = load_snapshot(&path);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].tabs.len(), 1);
        assert!(!groups[0].tabs[0].unread, "pre-migration rows load as unread=false");
        assert_eq!(
            groups[0].tabs[0].unread_at, None,
            "pre-migration rows have no attention stamp"
        );

        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
