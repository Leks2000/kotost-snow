fn kotost_snow_fill(region: &[[Option<&Chunk>; 3]; 3], sy: i32) {
    // A section is about to be meshed: where it is in the world.
    if let Some(c) = region[1][1] {
        crate::kotost_snow::section_origin([c.x * 16, c.min_y() + sy * 16, c.z * 16]);
    }
}
