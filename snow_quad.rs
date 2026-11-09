fn kotost_snow_quad(
    mesher: &Mesher<'_>,
    out: &mut SectionMesh,
    solid_leaves: bool,
    tile: TileLayer,
    v: [TerrainVertex; 4],
    tint: Option<u32>,
) -> std::ops::ControlFlow<()> {
    use crate::kotost_snow as ks;
    use std::ops::ControlFlow::{Break, Continue};
    // Our own pieces come back through here: emit them as they are.
    if ks::nested() || !ks::section_active() {
        return Continue(());
    }
    let TileLayer::Layer { layer, .. } = tile else {
        return Continue(());
    };
    let assets = mesher.render as *const _ as usize;
    let sprite = ks::sprite_kind(assets, layer, |name| mesher.render.sprite_layers.get(name).map(|&l| l as u16));
    if sprite == 0 {
        return Continue(());
    }
    let Some(pieces) = ks::reshape(&v, sprite) else {
        return Continue(());
    };
    // A dead bush in snow wears the fern's (grey) sprite, frosted white.
    let tile = match (sprite, ks::frost_sprite(), tile) {
        (3, Some(fern), TileLayer::Layer { translucent, opaque, .. }) => TileLayer::Layer {
            layer: fern,
            translucent,
            opaque,
        },
        _ => tile,
    };
    ks::set_nested(true);
    for piece in pieces {
        mesher.quad(out, solid_leaves, tile, piece, tint);
    }
    ks::set_nested(false);
    Break(())
}
