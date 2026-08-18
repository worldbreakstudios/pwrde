//! Pure file-tree model, built from a flat list of full file paths.
//!
//! A later hand-written sidebar consumes this: it renders a GitHub-style file
//! tree next to the diff. The tree collapses single-child directory chains into
//! one node (so `a/b/c/file` with no siblings along the way shows as one
//! `a/b/c` node), and sorts directories before files, each level ordered
//! case-insensitively by name. Like `diff.rs`, this is pure data with no gpui
//! dependency so it can be unit-tested in isolation.

use std::collections::BTreeMap;

/// A node in the file tree: either a directory (with children) or a file leaf.
#[derive(Clone, Debug, PartialEq)]
pub enum FileTreeNode {
    Dir {
        name: String,
        path: String,
        children: Vec<FileTreeNode>,
    },
    File {
        name: String,
        path: String,
    },
}

/// Intermediate mutable tree used while inserting paths, before finalizing into
/// the immutable `FileTreeNode` form (with chain collapsing + sorting).
#[derive(Default)]
struct DirBuilder {
    /// Child directories, keyed by their (single-segment) name.
    dirs: BTreeMap<String, DirBuilder>,
    /// Leaf files in this directory, keyed by their (single-segment) name.
    files: BTreeMap<String, ()>,
}

impl DirBuilder {
    /// Insert a full path's segments, placing the file at its leaf directory.
    fn insert(&mut self, segments: &[&str]) {
        match segments {
            [] => {}
            [file] => {
                self.files.insert((*file).to_string(), ());
            }
            [dir, rest @ ..] => {
                self.dirs
                    .entry((*dir).to_string())
                    .or_default()
                    .insert(rest);
            }
        }
    }

    /// Finalize this directory's children into sorted `FileTreeNode`s, collapsing
    /// single-child directory chains. `prefix` is the accumulated path of this
    /// directory (used to build each child's full `path`).
    fn finalize(self, prefix: &str) -> Vec<FileTreeNode> {
        let mut dirs: Vec<FileTreeNode> = self
            .dirs
            .into_iter()
            .map(|(name, builder)| builder.into_node(name, prefix))
            .collect();
        let mut files: Vec<FileTreeNode> = self
            .files
            .into_keys()
            .map(|name| {
                let path = join_path(prefix, &name);
                FileTreeNode::File { name, path }
            })
            .collect();

        dirs.sort_by(|a, b| node_name(a).to_lowercase().cmp(&node_name(b).to_lowercase()));
        files.sort_by(|a, b| node_name(a).to_lowercase().cmp(&node_name(b).to_lowercase()));

        // Directories sort before files; each group is already name-sorted.
        dirs.into_iter().chain(files).collect()
    }

    /// Turn a directory builder into a `Dir` node, collapsing single-child dir
    /// chains: a dir with no files and exactly one child dir merges with that
    /// child (name = "parent/child", path = the deeper path).
    fn into_node(mut self, mut name: String, prefix: &str) -> FileTreeNode {
        let mut path = join_path(prefix, &name);

        // Collapse: while this dir holds no files and exactly one child dir,
        // fold that child into this node.
        while self.files.is_empty() && self.dirs.len() == 1 {
            let (child_name, child) = self.dirs.into_iter().next().unwrap();
            name = format!("{name}/{child_name}");
            path = join_path(prefix, &name);
            self = child;
        }

        let children = self.finalize(&path);
        FileTreeNode::Dir {
            name,
            path,
            children,
        }
    }
}

/// The display name of a node, for sorting.
fn node_name(node: &FileTreeNode) -> &str {
    match node {
        FileTreeNode::Dir { name, .. } => name,
        FileTreeNode::File { name, .. } => name,
    }
}

/// Join a path prefix and a segment with '/', avoiding a leading slash at root.
fn join_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

/// Build a GitHub-style tree from full file paths. Collapses single-child
/// directory chains into one node (e.g. `a/b/c` with one child dir becomes a
/// node named "a/b/c"), and sorts directories before files, each
/// case-insensitively by name.
pub fn build_file_tree(paths: &[String]) -> Vec<FileTreeNode> {
    let mut root = DirBuilder::default();
    for path in paths {
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if !segments.is_empty() {
            root.insert(&segments);
        }
    }
    root.finalize("")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_single_child_chain() {
        // `a/b/c/file.rs` has no siblings anywhere along the chain, so the whole
        // `a/b/c` prefix collapses into one directory node.
        let tree = build_file_tree(&["a/b/c/file.rs".to_string()]);
        assert_eq!(
            tree,
            vec![FileTreeNode::Dir {
                name: "a/b/c".to_string(),
                path: "a/b/c".to_string(),
                children: vec![FileTreeNode::File {
                    name: "file.rs".to_string(),
                    path: "a/b/c/file.rs".to_string(),
                }],
            }]
        );
    }

    #[test]
    fn dirs_before_files() {
        // At the root, the `zdir` directory must sort before the `afile.txt`
        // file even though 'z' > 'a', because dirs always precede files.
        let tree = build_file_tree(&["afile.txt".to_string(), "zdir/inner.rs".to_string()]);
        assert_eq!(
            tree,
            vec![
                FileTreeNode::Dir {
                    name: "zdir".to_string(),
                    path: "zdir".to_string(),
                    children: vec![FileTreeNode::File {
                        name: "inner.rs".to_string(),
                        path: "zdir/inner.rs".to_string(),
                    }],
                },
                FileTreeNode::File {
                    name: "afile.txt".to_string(),
                    path: "afile.txt".to_string(),
                },
            ]
        );
    }

    #[test]
    fn mixed_dir_with_two_files() {
        // A directory with two files must NOT collapse (it has files), and its
        // two files sort case-insensitively by name.
        let tree = build_file_tree(&[
            "src/Beta.rs".to_string(),
            "src/alpha.rs".to_string(),
        ]);
        assert_eq!(
            tree,
            vec![FileTreeNode::Dir {
                name: "src".to_string(),
                path: "src".to_string(),
                children: vec![
                    FileTreeNode::File {
                        name: "alpha.rs".to_string(),
                        path: "src/alpha.rs".to_string(),
                    },
                    FileTreeNode::File {
                        name: "Beta.rs".to_string(),
                        path: "src/Beta.rs".to_string(),
                    },
                ],
            }]
        );
    }
}
