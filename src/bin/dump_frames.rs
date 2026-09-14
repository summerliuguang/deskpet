/// 调试工具（仅本机预览用）：把所有精灵帧导出成 PPM 图片。
/// 运行：cargo run --bin dump_frames，输出到 /tmp/pet_frames/
use deskpet::model::{make_model, PetState};
use deskpet::sprites::{self, COSTUMES};

fn main() {
    let mut model = make_model(0);
    let cases: &[(&str, PetState, (i32, i32), usize, usize)] = &[
        ("1_idle_open", PetState::Idle, (0, 0), 0, 0),
        ("1b_idle_open_gaze_r", PetState::Idle, (1, 0), 0, 0),
        ("1c_idle_typing", PetState::Idle, (0, 0), 0, 0),
        ("2_idle_blink", PetState::Idle, (0, 0), 0, 0),
        ("3_happy", PetState::Patted, (0, 0), 0, 0),
        ("4_shock", PetState::Shocked, (0, 0), 0, 0),
        ("5_walk_a", PetState::Walk, (1, 0), 0, 0),
        ("6_walk_b", PetState::Walk, (1, 0), 0, 0),
        ("7_dragged_a", PetState::Dragged, (0, 0), 0, 0),
        ("8_thrown", PetState::Thrown, (0, 0), 0, 0),
        ("9_sleep", PetState::Sleep, (0, 0), 0, 0),
        ("10_climb_left", PetState::Climb, (0, 0), 0, 0),
        ("11_costume_black", PetState::Idle, (0, 0), 0, 2),
        ("12_expr_happy_pinned", PetState::Idle, (0, 0), 1, 0),
    ];
    std::fs::create_dir_all("/tmp/pet_frames").unwrap();
    for (name, state, gaze, expr, costume) in cases {
        model.set_costume(*costume);
        model.set_expression(*expr);
        let pose = deskpet::model::Pose {
            state: *state,
            tick: 0,
            gaze: *gaze,
            expr: *expr,
            costume: *costume,
            aux: if *state == PetState::Climb { -1 } else { 0 },
            typing: name.contains("typing"),
        };
        let rgb = sprites::argb_to_rgb(&model.render(&pose));
        let mut out =
            format!("P6\n{} {}\n255\n", model.size().0, model.size().1).into_bytes();
        out.extend_from_slice(&rgb);
        std::fs::write(format!("/tmp/pet_frames/{name}.ppm"), out).unwrap();
    }
    // 换装配色一览
    let names: Vec<String> = COSTUMES.iter().map(|c| c.name.to_string()).collect();
    // 文本渲染预览：气泡底 + 新像素字体
    let (w, h) = (240usize, 72usize);
    let mut buf = vec![0xFFFDF9EEu32; w * h];
    for y in 0..h {
        for x in 0..w {
            let edge = x < 2 || x >= w - 2 || y < 2 || y >= h - 2;
            buf[y * w + x] = if edge { 0xFF5A5040 } else { buf[y * w + x] };
        }
    }
    deskpet::text::TEXT.draw(
        &mut buf, w as i32, h as i32, 8, 6,
        &["喵呜！你好呀～".to_string(), "今天也要加油鸭 Hello!".to_string()],
        (45, 42, 38), false,
    );
    let mut out = format!("P6\n{} {}\n255\n", w, h).into_bytes();
    for p in &buf {
        out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
    }
    std::fs::write("/tmp/pet_frames/99_text_preview.ppm", out).unwrap();

    // 四个物种预览（走路帧）
    for kind in 0..4usize {
        let mut m = make_model(kind);
        let walk = PetState::Walk;
        let pose = deskpet::model::Pose {
            state: walk, tick: 0, gaze: (0, 0), expr: 0, costume: 0, aux: 0, typing: false,
        };
        let rgb = sprites::argb_to_rgb(&m.render(&pose));
        let mut out = format!("P6\n{} {}\n255\n", m.size().0, m.size().1).into_bytes();
        out.extend_from_slice(&rgb);
        std::fs::write(format!("/tmp/pet_frames/species_{kind}.ppm"), out).unwrap();
    }
    println!(" costumes: {:?}", names);
    println!("done: /tmp/pet_frames/*.ppm");
}
