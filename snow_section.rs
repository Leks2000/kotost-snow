fn kotost_snow_section(pad: &Padded, solid: &[bool]) {
    use crate::kotost_snow as ks;
    // A section starts meshing (the padded blocks are filled): note its snow and walls for the
    // snow models built while it is meshed.
    if !ks::kinds_ready() || !ks::tune().enabled {
        ks::section_classes(None);
        return;
    }
    let mut any = false;
    let mut classes = Box::new([0u8; ks::SECTION_CELLS]);
    for y in -2..=17 {
        for z in -2..=17 {
            for x in -2..=17 {
                let id = pad.blocks[pi(x, y, z)];
                let k = ks::kind(id);
                let c = if ks::is_snow(k) {
                    if (0..16).contains(&x) && (0..16).contains(&y) && (0..16).contains(&z) {
                        any = true;
                    }
                    k
                } else if k == ks::PLANT_KIND {
                    ks::PLANT
                } else if solid.get(id as usize).copied().unwrap_or(false) {
                    ks::WALL
                } else {
                    0
                };
                classes[ks::cell_index(x, y, z)] = c;
            }
        }
    }
    ks::section_classes(if any { Some(classes) } else { None });
}
