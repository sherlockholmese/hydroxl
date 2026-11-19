use super::primitives::ledger;
use crate::AboutAction;
use crate::app::Workspace;
use crate::theme::Palette;
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::*;
use gpui_kit::{prelude::*, *};

const SOURCE_URL: &str = "https://github.com/sherlockholmese/hydroxl";
const APP_ICON: &[u8] = include_bytes!("../../packaging/dev.hydroxl.Hydroxl.svg");

impl Workspace {
    pub(crate) fn about(&mut self, _: &AboutAction, window: &mut Window, cx: &mut Context<Self>) {
        let qemu = if crate::backend::qemu::bundle_installed() {
            format!("{} with Neptune", env!("HYDROXL_QEMU_VERSION"))
        } else {
            "Not installed".to_owned()
        };
        let icon = std::sync::Arc::new(Image::from_bytes(ImageFormat::Svg, APP_ICON.to_vec()));
        window.open_dialog(cx, move |dialog, _, cx| {
            let p = Palette::get(cx);
            crate::forms::frame(dialog, "Hydroxl", 440.)
                .child(
                    h_flex()
                        .items_start()
                        .gap_5()
                        .text_color(p.text)
                        .child(img(icon.clone()).size(px(64.)).flex_shrink_0())
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(ledger("Version", env!("CARGO_PKG_VERSION"), p))
                                .child(ledger("Bundled QEMU", qemu.clone(), p))
                                .child(ledger("Triton driver", env!("HYDROXL_TRITON_VERSION"), p))
                                .child(ledger("License", env!("CARGO_PKG_LICENSE"), p)),
                        ),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            Button::new("about-source")
                                .ghost()
                                .label("Source code")
                                .on_click(|_, _, cx| cx.open_url(SOURCE_URL)),
                        )
                        .child(div().flex_1())
                        .child(
                            Button::new("about-done")
                                .outline()
                                .label("Done")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        ),
                )
        });
    }
}
