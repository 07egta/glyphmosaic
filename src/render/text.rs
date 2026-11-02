fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        '0' => [
            0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110,
        ],
        '1' => [
            0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110,
        ],
        '2' => [
            0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111,
        ],
        '3' => [
            0b11111, 0b00010, 0b00100, 0b00010, 0b00001, 0b10001, 0b01110,
        ],
        '4' => [
            0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010,
        ],
        '5' => [
            0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110,
        ],
        '6' => [
            0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110,
        ],
        '7' => [
            0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000,
        ],
        '8' => [
            0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110,
        ],
        '9' => [
            0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100,
        ],
        'A' => [
            0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001,
        ],
        'B' => [
            0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110,
        ],
        'C' => [
            0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110,
        ],
        'D' => [
            0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110,
        ],
        'E' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111,
        ],
        'F' => [
            0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000,
        ],
        'U' => [
            0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110,
        ],
        '+' => [
            0b00000, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0b00000,
        ],
        '-' => [
            0b00000, 0b00000, 0b00000, 0b11111, 0b00000, 0b00000, 0b00000,
        ],
        ':' => [
            0b00000, 0b01100, 0b01100, 0b00000, 0b01100, 0b01100, 0b00000,
        ],
        _ => [0; 7],
    }
}

pub fn text_width(text: &str, scale: u32) -> u32 {
    let chars = text.chars().count() as u32;
    if chars == 0 || scale == 0 {
        0
    } else {
        chars * 6 * scale - scale
    }
}

#[allow(clippy::too_many_arguments)]
pub fn draw_text(
    canvas: &mut [u8],
    cw: u32,
    ch: u32,
    x: i32,
    y: i32,
    scale: u32,
    text: &str,
    color: [u8; 3],
) {
    assert_eq!(canvas.len(), cw as usize * ch as usize * 3);
    if scale == 0 {
        return;
    }
    let scale = scale as i32;
    for (index, c) in text.chars().enumerate() {
        let rows = glyph(c);
        let origin_x = x + index as i32 * 6 * scale;
        for (row, bits) in rows.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 {
                    continue;
                }
                let px0 = origin_x + col * scale;
                let py0 = y + row as i32 * scale;
                for dy in 0..scale {
                    let py = py0 + dy;
                    if py < 0 || py >= ch as i32 {
                        continue;
                    }
                    for dx in 0..scale {
                        let px = px0 + dx;
                        if px < 0 || px >= cw as i32 {
                            continue;
                        }
                        let i = (py as usize * cw as usize + px as usize) * 3;
                        canvas[i..i + 3].copy_from_slice(&color);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_is_monospace() {
        assert_eq!(text_width("U+2044", 3), 6 * 6 * 3 - 3);
    }

    #[test]
    fn draw_sets_pixels() {
        let mut canvas = vec![0u8; 100 * 20 * 3];
        draw_text(&mut canvas, 100, 20, 2, 2, 3, "U+2044", [255, 255, 255]);
        assert!(canvas.chunks(3).any(|p| p == [255, 255, 255]));
        assert!(canvas.chunks(3).any(|p| p == [0, 0, 0]));
    }
}
