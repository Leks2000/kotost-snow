fn kotost_snow_fog(fog_type: horizon_client::fog::FogType, fog: &mut horizon_client::fog::FogData) {
    // A blizzard closes in: white haze, the world, the sky and the clouds fade out a few dozen blocks away.
    // Off (`enabled = 0`): no haze, even if it was thick when the mod was turned off.
    if !horizon_client_render::kotost_snow::tune().enabled {
        return;
    }
    let h = horizon_client_render::kotost_snow::haze().clamp(0.0, 1.0);
    if h < 0.01 || !matches!(fog_type, horizon_client::fog::FogType::Atmospheric) {
        return;
    }
    let near = |far: f32| far + (1.0 - h) * 400.0;
    fog.environmental_start = fog.environmental_start.min(near(2.0));
    fog.environmental_end = fog.environmental_end.min(near(42.0));
    fog.render_distance_start = fog.render_distance_start.min(near(4.0));
    fog.render_distance_end = fog.render_distance_end.min(near(80.0));
    fog.sky_end = fog.sky_end.min(near(60.0));
    fog.cloud_end = fog.cloud_end.min(near(40.0));
    let white = [0.80f32, 0.83, 0.88];
    for k in 0..3 {
        fog.color[k] += (white[k] - fog.color[k]) * h * 0.8;
    }
}
