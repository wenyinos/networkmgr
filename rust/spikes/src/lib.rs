//! 两个 GUI spike 共用的最小资源：不依赖图形库，方便在 A 类环境单测。

/// 生成 32×32 的不透明 RGBA 图标（蓝色圆角方块）。
///
/// `tray-icon` 的 `Icon::from_rgba` 只接受原始 RGBA 字节，这里避免为了一个
/// spike 引入 `image` crate 或外部 PNG 文件。
pub fn icon_rgba() -> Vec<u8> {
    const SIZE: u32 = 32;
    let mut pixels = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let edge = x < 2 || y < 2 || x >= SIZE - 2 || y >= SIZE - 2;
            let (r, g, b) = if edge { (20, 40, 90) } else { (60, 130, 230) };
            pixels.extend_from_slice(&[r, g, b, 255]);
        }
    }
    pixels
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_is_32x32_opaque_rgba() {
        let pixels = icon_rgba();
        assert_eq!(pixels.len(), 32 * 32 * 4);
        assert!(pixels.chunks_exact(4).all(|p| p[3] == 255));
    }
}
