//! Headless API regression: no desktop session or VM is started.
use gpui_kit::prelude::*;
use gpui_kit::{
    AppContext, Bounds, Context, DevicePixels, EmptyView, IntoElement, Point, Render, TextureId,
    Window, WindowOptions, px, size,
};

struct TextureView(TextureId);
impl Render for TextureView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        gpui_kit::texture(self.0).w(px(400.)).h(px(300.))
    }
}

#[test]
fn texture_api_validates_ownership_and_headless_fallback() {
    gpui_kit::platform::headless().run(|cx| {
        let mut id = None;
        let first = cx
            .open_window(WindowOptions::default(), |window, cx| {
                let texture = window
                    .create_texture(size(DevicePixels(3), DevicePixels(2)), &[0; 24])
                    .unwrap();
                id = Some(texture);
                cx.new(|_| TextureView(texture))
            })
            .unwrap();
        let id = id.unwrap();
        let second =
            cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| EmptyView)).unwrap();
        second
            .update(cx, |_, window, _| {
                let rect = Bounds::new(Point::default(), size(DevicePixels(1), DevicePixels(1)));
                assert!(window.update_texture(id, rect, &[0; 4]).is_err());
                window.drop_texture(id);
            })
            .unwrap();
        cx.update_window(first.into(), |_, window, cx| {
            let rect = Bounds::new(
                Point {
                    x: DevicePixels(1),
                    y: DevicePixels(0),
                },
                size(DevicePixels(1), DevicePixels(2)),
            );
            window.update_texture(id, rect, &[1; 8]).unwrap();
            let source = [
                91, 92, 93, 94, 11, 13, 17, 255, 95, 96, 97, 98, 99, 100, 101, 102, 19, 23, 29, 255,
            ];
            window.update_texture_with_stride(id, rect, &source[4..], 12).unwrap();
            assert!(window.update_texture_with_stride(id, rect, &source[4..19], 12).is_err());
            assert!(window.update_texture_with_stride(id, rect, &source, 3).is_err());
            assert!(window.update_texture_with_stride(id, rect, &source, usize::MAX).is_err());
            window.draw(cx).clear(cx); // HeadlessAtlas falls back to rebuilding from the CPU copy.
            assert!(window.update_texture(id, rect, &[9; 7]).is_err());
            let outside = Bounds::new(
                Point {
                    x: DevicePixels(i32::MAX),
                    y: DevicePixels(0),
                },
                rect.size,
            );
            assert!(window.update_texture(id, outside, &[9; 8]).is_err());
            for _ in 0..1000 {
                window.update_texture(id, rect, &[2; 8]).unwrap();
            }
            window.draw(cx).clear(cx);
            assert!(!window.is_pointer_locked());
            assert!(window.lock_pointer(true).is_err());
            assert!(window.inhibit_system_shortcuts(true).is_err());
            assert!(window.set_cursor_hidden(true).is_err());
            assert!(window.lock_pointer(false).is_ok());
            assert!(window.inhibit_system_shortcuts(false).is_ok());
            assert!(window.set_cursor_hidden(false).is_ok());
            assert_eq!(window.take_raw_mouse_motion(), Point::default());
            window.drop_texture(id);
            assert!(window.update_texture(id, rect, &[0; 8]).is_err());
            assert!(window.create_texture(size(DevicePixels(-1), DevicePixels(2)), &[]).is_err());
        })
        .unwrap();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(std::time::Duration::from_millis(1)).await;
            cx.update(|cx| cx.quit());
        })
        .detach();
    });
}
