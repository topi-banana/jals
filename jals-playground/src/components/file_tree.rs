//! The workspace files sidebar.

use yew::prelude::*;

use super::PANE_LABEL;

/// One flattened row of the workspace file tree — pre-order, with an indentation `depth`.
///
/// The root [`App`] flattens its [`Workspace`] into a `Vec<TreeEntry>` so this component stays
/// presentational and Yew-agnostic about the file store.
///
/// [`App`]: crate::app::App
/// [`Workspace`]: crate::workspace::Workspace
#[derive(Clone, PartialEq)]
pub struct TreeEntry {
    /// Full `/`-separated path — the selection key and the active-file comparison.
    pub path: String,
    /// The last path segment, shown in the row.
    pub name: String,
    /// Indentation depth (0 = a top-level entry).
    pub depth: usize,
    /// Whether this row is a directory (a non-clickable label) or a file (selectable).
    pub is_dir: bool,
}

/// Props for [`FileTree`].
#[derive(Properties, PartialEq)]
pub struct FileTreeProps {
    /// The workspace files, flattened pre-order (see [`TreeEntry`]).
    pub entries: Vec<TreeEntry>,
    /// Path of the active file — its row carries the app-shell left-edge indicator.
    pub active: String,
    /// Emitted with a file's path when its row is clicked.
    pub on_select: Callback<String>,
}

/// The file-tree sidebar: the workspace's files rendered as a fully-expanded tree, each file row
/// selecting the active file. The active row carries the app-shell left-edge indicator.
pub struct FileTree;

impl Component for FileTree {
    type Message = ();
    type Properties = FileTreeProps;

    fn create(_ctx: &Context<Self>) -> Self {
        FileTree
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let props = ctx.props();
        html! {
            <aside class="flex w-60 shrink-0 flex-col overflow-auto border-r border-hairline bg-canvas">
                <div class={PANE_LABEL}>{ "Files" }</div>
                <div class="py-1">
                    { for props.entries.iter().map(|entry| self.view_entry(ctx, entry)) }
                </div>
            </aside>
        }
    }
}

impl FileTree {
    /// A single tree row: a directory label or a clickable file, indented by its `depth`.
    fn view_entry(&self, ctx: &Context<Self>, entry: &TreeEntry) -> Html {
        let pad = format!("padding-left: {}px", 12 + entry.depth * 14);
        if entry.is_dir {
            html! {
                <div class="flex items-center gap-1 py-1 font-mono text-xs text-mute" style={pad}>
                    <span>{ "▸" }</span>
                    <span>{ entry.name.clone() }</span>
                </div>
            }
        } else {
            let is_active = entry.path == ctx.props().active;
            let onclick = {
                let path = entry.path.clone();
                ctx.props().on_select.reform(move |_| path.clone())
            };
            let base = "flex cursor-pointer items-center gap-1 py-1 font-mono text-xs";
            let state = if is_active {
                "border-l-2 border-ink bg-canvas-soft text-ink"
            } else {
                "border-l-2 border-transparent text-body hover:bg-canvas-soft"
            };
            html! {
                <div class={classes!(base, state)} style={pad} onclick={onclick}>
                    <span>{ entry.name.clone() }</span>
                </div>
            }
        }
    }
}
