//! The sequence diagram of a trace. The markdown view already draws Mermaid with zoom, scrolling,
//! the theme and a button that copies the source, so this view hands it the text in a code block.

use gpui::{AppContext as _, Context, Entity, IntoElement, Render, SharedString, Window};
use markdown::{
    CodeBlockRenderer, CopyButtonVisibility, Markdown, MarkdownElement, MarkdownFont,
    MarkdownOptions, MarkdownStyle, WrapButtonVisibility,
};
use ui::prelude::*;

pub(crate) struct SequenceView {
    markdown: Entity<Markdown>,
    mermaid: SharedString,
}

impl SequenceView {
    pub(crate) fn new(mermaid: String, cx: &mut Context<Self>) -> Self {
        let source = fenced(&mermaid);
        let markdown = cx.new(|cx| {
            Markdown::new_with_options(
                source.into(),
                None,
                None,
                MarkdownOptions {
                    render_mermaid_diagrams: true,
                    ..Default::default()
                },
                cx,
            )
        });
        Self {
            markdown,
            mermaid: mermaid.into(),
        }
    }

    /// The Mermaid text, for the Copy action.
    pub(crate) fn mermaid(&self) -> &SharedString {
        &self.mermaid
    }
}

/// A code block whose fence is longer than any run of backticks in the text, so nothing in a
/// trace's messages can end the block early.
fn fenced(mermaid: &str) -> String {
    let longest_run = mermaid
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat((longest_run + 1).max(3));
    format!("{fence}mermaid\n{mermaid}\n{fence}\n")
}

impl Render for SequenceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = MarkdownStyle::themed(MarkdownFont::Preview, window, cx);
        div()
            .id("sequence-view")
            .size_full()
            .overflow_y_scroll()
            .p_3()
            .child(
                MarkdownElement::new(self.markdown.clone(), style).code_block_renderer(
                    CodeBlockRenderer::Default {
                        copy_button_visibility: CopyButtonVisibility::VisibleOnHover,
                        wrap_button_visibility: WrapButtonVisibility::Hidden,
                        border: false,
                    },
                ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fence_outgrows_any_backticks_in_the_text() {
        assert_eq!(fenced("a"), "```mermaid\na\n```\n");
        assert_eq!(fenced("a ``` b"), "````mermaid\na ``` b\n````\n");
        assert_eq!(fenced("`x` ````` y"), "``````mermaid\n`x` ````` y\n``````\n");
    }
}
