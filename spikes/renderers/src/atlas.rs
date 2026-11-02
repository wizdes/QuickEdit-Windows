//! Places glyph bitmaps in the atlas texture, in rows from the top.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Place {
    pub x: u32,
    pub y: u32,
}

pub struct ShelfPacker {
    side: u32,
    x: u32,
    y: u32,
    row_height: u32,
}

impl ShelfPacker {
    pub fn new(side: u32) -> Self {
        ShelfPacker {
            side,
            x: 0,
            y: 0,
            row_height: 0,
        }
    }

    /// Finds room for a bitmap, with one blank texel around it. `None` means
    /// the atlas is full.
    pub fn place(&mut self, width: u32, height: u32) -> Option<Place> {
        if width + 1 > self.side || height + 1 > self.side {
            return None;
        }
        if self.x + width + 1 > self.side {
            self.y += self.row_height;
            self.x = 0;
            self.row_height = 0;
        }
        if self.y + height + 1 > self.side {
            return None;
        }
        let place = Place {
            x: self.x,
            y: self.y,
        };
        self.x += width + 1;
        self.row_height = self.row_height.max(height + 1);
        Some(place)
    }
}

/// A square that holds at least 256 glyph cells of this size.
pub fn atlas_side(cell: i32, line: i32) -> u32 {
    let largest = cell.max(line).max(1) as u32;
    (16 * (largest + 2)).next_power_of_two()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn places_in_a_row_then_wraps_without_overlap() {
        let mut packer = ShelfPacker::new(10);
        assert_eq!(packer.place(4, 3), Some(Place { x: 0, y: 0 }));
        assert_eq!(packer.place(4, 2), Some(Place { x: 5, y: 0 }));
        assert_eq!(packer.place(4, 2), Some(Place { x: 0, y: 4 }));
    }

    #[test]
    fn says_when_the_atlas_is_full() {
        let mut packer = ShelfPacker::new(10);
        assert_eq!(packer.place(20, 2), None);
        assert!(packer.place(9, 9).is_some());
        assert_eq!(packer.place(9, 9), None);
    }

    #[test]
    fn atlas_holds_256_cells() {
        let (cell, line) = (8, 18);
        let side = atlas_side(cell, line);
        let mut packer = ShelfPacker::new(side);
        for _ in 0..256 {
            assert!(packer.place(cell as u32, line as u32).is_some());
        }
    }
}
