//! The formatter settings bar under the header.

use jals_fmt::{Config, IndentStyle, LineEnding};
use web_sys::{HtmlInputElement, HtmlSelectElement};
use yew::prelude::*;

/// Props for [`SettingsBar`].
#[derive(Properties, PartialEq)]
pub struct SettingsBarProps {
    /// The current formatter configuration to render.
    pub config: Config,
    /// Emitted with a fresh [`Config`] whenever a control changes.
    pub on_change: Callback<Config>,
}

/// The formatter settings bar. Each control emits a fresh [`Config`] via [`SettingsBarProps::on_change`];
/// `onchange` (not `oninput`) keeps the numeric fields stable while typing, and a parse failure
/// leaves the field unchanged. The bar itself is stateless — the root [`App`] owns the config and
/// feeds the latest value back down as [`SettingsBarProps::config`].
///
/// [`App`]: crate::app::App
pub struct SettingsBar;

impl Component for SettingsBar {
    type Message = ();
    type Properties = SettingsBarProps;

    fn create(_ctx: &Context<Self>) -> Self {
        SettingsBar
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let props = ctx.props();
        let cfg = props.config.clone();
        let on_change = props.on_change.clone();

        // A change callback for one `usize` field, identified by a setter closure.
        let usize_cb = |set: fn(&mut Config, usize)| {
            let cfg = cfg.clone();
            let on_change = on_change.clone();
            Callback::from(move |e: Event| {
                let el: HtmlInputElement = e.target_unchecked_into();
                let mut c = cfg.clone();
                if let Ok(value) = el.value().parse::<usize>() {
                    set(&mut c, value);
                }
                on_change.emit(c);
            })
        };
        // A change callback for one `bool` field (a checkbox).
        let bool_cb = |set: fn(&mut Config, bool)| {
            let cfg = cfg.clone();
            let on_change = on_change.clone();
            Callback::from(move |e: Event| {
                let el: HtmlInputElement = e.target_unchecked_into();
                let mut c = cfg.clone();
                set(&mut c, el.checked());
                on_change.emit(c);
            })
        };
        // A change callback for one `<select>`, mapping its selected value to a field.
        let select_cb = |set: fn(&mut Config, String)| {
            let cfg = cfg.clone();
            let on_change = on_change.clone();
            Callback::from(move |e: Event| {
                let el: HtmlSelectElement = e.target_unchecked_into();
                let mut c = cfg.clone();
                set(&mut c, el.value());
                on_change.emit(c);
            })
        };

        let on_indent_style = select_cb(|c, v| {
            c.indent_style = if v == "tab" {
                IndentStyle::Tab
            } else {
                IndentStyle::Space
            };
        });
        let on_indent_width = usize_cb(|c, v| c.indent_width = v.max(1));
        let on_max_width = usize_cb(|c, v| c.max_width = v.max(1));
        let on_comment_width = usize_cb(|c, v| c.comment_width = v.max(1));
        let on_max_blank_lines = usize_cb(|c, v| c.max_blank_lines = v);
        let on_line_ending = select_cb(|c, v| {
            c.line_ending = if v == "crlf" {
                LineEnding::Crlf
            } else {
                LineEnding::Lf
            };
        });
        let on_final_newline = bool_cb(|c, v| c.insert_final_newline = v);
        let on_wrap_comments = bool_cb(|c, v| c.wrap_comments = v);

        let field = "flex items-center gap-1.5";
        let lbl = "font-mono text-xs text-mute";
        let num = "h-7 w-14 rounded-md border border-hairline bg-canvas px-2 text-xs text-ink outline-none";
        let sel =
            "h-7 rounded-md border border-hairline bg-canvas px-1 text-xs text-ink outline-none";

        html! {
            <div class="flex flex-wrap items-center gap-x-5 gap-y-2 border-b border-hairline bg-canvas-soft px-6 py-2">
                <label class={field}>
                    <span class={lbl}>{ "Indent" }</span>
                    <select class={sel} onchange={on_indent_style}>
                        <option value="space" selected={cfg.indent_style == IndentStyle::Space}>{ "Spaces" }</option>
                        <option value="tab" selected={cfg.indent_style == IndentStyle::Tab}>{ "Tabs" }</option>
                    </select>
                    <input class={num} type="number" min="1" value={cfg.indent_width.to_string()} onchange={on_indent_width} />
                </label>
                <label class={field}>
                    <span class={lbl}>{ "Max width" }</span>
                    <input class={num} type="number" min="1" value={cfg.max_width.to_string()} onchange={on_max_width} />
                </label>
                <label class="flex cursor-pointer items-center gap-1.5">
                    <input type="checkbox" class="h-3.5 w-3.5 accent-ink" checked={cfg.wrap_comments} onchange={on_wrap_comments} />
                    <span class={lbl}>{ "Wrap comments" }</span>
                </label>
                <label class={field}>
                    <span class={lbl}>{ "Comment width" }</span>
                    <input class={num} type="number" min="1" value={cfg.comment_width.to_string()} onchange={on_comment_width} />
                </label>
                <label class={field}>
                    <span class={lbl}>{ "Blank lines" }</span>
                    <input class={num} type="number" min="0" value={cfg.max_blank_lines.to_string()} onchange={on_max_blank_lines} />
                </label>
                <label class={field}>
                    <span class={lbl}>{ "Line ending" }</span>
                    <select class={sel} onchange={on_line_ending}>
                        <option value="lf" selected={cfg.line_ending == LineEnding::Lf}>{ "LF" }</option>
                        <option value="crlf" selected={cfg.line_ending == LineEnding::Crlf}>{ "CRLF" }</option>
                    </select>
                </label>
                <label class="flex cursor-pointer items-center gap-1.5">
                    <input type="checkbox" class="h-3.5 w-3.5 accent-ink" checked={cfg.insert_final_newline} onchange={on_final_newline} />
                    <span class={lbl}>{ "Final newline" }</span>
                </label>
            </div>
        }
    }
}
