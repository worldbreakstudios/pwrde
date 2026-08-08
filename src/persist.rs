//! Session persistence — snapshots group layouts and shpool sessions to SQLite.
//!
//! The default DB path is `<data_dir>/pwrde/state.db`. When the process is
//! launched from a linked git worktree (see [`crate::git::worktree_scope`]),
//! the path becomes `<data_dir>/pwrde/worktrees/<slug>/state.db` so each
//! worktree keeps an isolated session database. Primary checkouts and
//! non-git launches keep the unscoped default.

use rusqlite::{Connection, Result as SqlResult};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Serializable layout tree mirroring workspace::Node without live state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LayoutNode {
    Leaf { tile: usize },
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
}

/// A saved tab within a group.
#[derive(Debug, Clone, PartialEq)]
pub struct SavedTab {
    pub tile_id: usize,
    pub tab_index: usize,
    pub active: bool,
    pub shpool_session: Option<String>,
    pub cwd: Option<String>,
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
fn db_path() -> Option<PathBuf> {
    let data_dir = dirs::data_dir()?;
    let scope = crate::git::worktree_scope();
    Some(db_path_in(&data_dir, scope.as_deref()))
}

/// Open the DB at `path`, creating tables if needed. Never panics on corrupt DB.
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
    
    Ok(conn)
}

/// Save a snapshot to the DB, rewriting all groups in one transaction.
/// Returns Ok(()) on success, Err on DB failure. Never panics.
pub fn save_snapshot(groups: &[SavedGroup], path: &Path) -> SqlResult<()> {
    let conn = open_db(path)?;
    let tx = conn.unchecked_transaction()?;
    
    tx.execute("DELETE FROM tabs", [])?;
    tx.execute("DELETE FROM groups", [])?;
    
    for group in groups {
        let layout_json = serde_json::to_string(&group.layout)
            .unwrap_or_else(|e| {
                eprintln!("persist: failed to serialize layout: {}", e);
                String::from("{}")
            });
        
        tx.execute(
            "INSERT INTO groups (position, name, cwd, focused_tile, layout)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            (
                group.position,
                &group.name,
                &group.cwd,
                group.focused_tile,
                &layout_json,
            ),
        )?;
        
        let group_id = tx.last_insert_rowid();
        
        for tab in &group.tabs {
            tx.execute(
                "INSERT INTO tabs (group_id, tile_id, tab_index, active, shpool_session, cwd)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (
                    group_id,
                    tab.tile_id,
                    tab.tab_index,
                    if tab.active { 1 } else { 0 },
                    &tab.shpool_session,
                    &tab.cwd,
                ),
            )?;
        }
    }
    
    tx.commit()?;
    Ok(())
}

/// Load the snapshot from the DB, returning saved groups.
/// Returns empty Vec on missing/corrupt DB. Never panics.
pub fn load_snapshot(path: &Path) -> Vec<SavedGroup> {
    let conn = match open_db(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("persist: failed to open DB: {}", e);
            return Vec::new();
        }
    };
    
    let mut stmt = match conn.prepare(
        "SELECT id, position, name, cwd, focused_tile, layout FROM groups ORDER BY position"
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
        let (group_id, position, name, cwd, focused_tile, layout_json) = match row_result {
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
            "SELECT tile_id, tab_index, active, shpool_session, cwd 
             FROM tabs WHERE group_id = ?1 ORDER BY tile_id, tab_index"
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
        });
    }
    
    groups
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
            }
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
                });
            }
            LayoutNode::Leaf { tile: tile.id as usize }
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
pub fn save_snapshot_default(groups: &[SavedGroup]) -> SqlResult<()> {
    match db_path() {
        Some(path) => save_snapshot(groups, &path),
        None => {
            eprintln!("persist: cannot determine data directory");
            Ok(())
        }
    }
}

/// Public API using the default DB path.
pub fn load_snapshot_default() -> Vec<SavedGroup> {
    match db_path() {
        Some(path) => load_snapshot(&path),
        None => {
            eprintln!("persist: cannot determine data directory");
            Vec::new()
        }
    }
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
                    a: Box::new(LayoutNode::Leaf { tile: 1 }),
                    b: Box::new(LayoutNode::Leaf { tile: 2 }),
                },
                tabs: vec![
                    SavedTab {
                        tile_id: 1,
                        tab_index: 0,
                        active: true,
                        shpool_session: Some("pwrde-1-abc".into()),
                        cwd: Some("/home/user".into()),
                    },
                    SavedTab {
                        tile_id: 1,
                        tab_index: 1,
                        active: false,
                        shpool_session: Some("pwrde-2-def".into()),
                        cwd: Some("/tmp".into()),
                    },
                    SavedTab {
                        tile_id: 2,
                        tab_index: 0,
                        active: true,
                        shpool_session: None,
                        cwd: None,
                    },
                ],
            },
            SavedGroup {
                position: 1,
                name: "scratch".into(),
                cwd: None,
                focused_tile: 3,
                layout: LayoutNode::Leaf { tile: 3 },
                tabs: vec![
                    SavedTab {
                        tile_id: 3,
                        tab_index: 0,
                        active: true,
                        shpool_session: Some("pwrde-3-xyz".into()),
                        cwd: Some("/var/log".into()),
                    },
                ],
            },
        ];
        
        save_snapshot(&groups, &path).unwrap();
        let loaded = load_snapshot(&path);
        
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].name, "main");
        assert_eq!(loaded[0].position, 0);
        assert_eq!(loaded[0].focused_tile, 1);
        assert_eq!(loaded[0].cwd, Some("/home/user".into()));
        
        // Verify split structure
        match &loaded[0].layout {
            LayoutNode::Split { dir, ratio, a, b } => {
                assert_eq!(dir, "row");
                assert!((ratio - 0.3).abs() < 0.001);
                assert_eq!(**a, LayoutNode::Leaf { tile: 1 });
                assert_eq!(**b, LayoutNode::Leaf { tile: 2 });
            }
            _ => panic!("expected split layout"),
        }
        
        // Verify tabs
        assert_eq!(loaded[0].tabs.len(), 3);
        assert_eq!(loaded[0].tabs[0].tile_id, 1);
        assert_eq!(loaded[0].tabs[0].tab_index, 0);
        assert_eq!(loaded[0].tabs[0].active, true);
        assert_eq!(loaded[0].tabs[0].shpool_session, Some("pwrde-1-abc".into()));
        assert_eq!(loaded[0].tabs[1].tile_id, 1);
        assert_eq!(loaded[0].tabs[1].tab_index, 1);
        assert_eq!(loaded[0].tabs[1].shpool_session, Some("pwrde-2-def".into()));
        
        assert_eq!(loaded[1].name, "scratch");
        assert_eq!(loaded[1].position, 1);
        assert_eq!(loaded[1].tabs.len(), 1);
        
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
    
    #[test]
    fn missing_db_loads_empty() {
        let groups = load_snapshot(Path::new("/nonexistent/pwrde/state.db"));
        assert_eq!(groups.len(), 0);
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
}
