//! 导出示例帧序列模型到 models/示例猫/：
//! 既是内置示例（证明帧序列加载链路），也是自制模型的模板——
//! 用 Aseprite 等工具画出同规格 PNG 序列，或用 Live2D Viewer
//! 把 Live2D 动作预渲染导出为透明帧，按此目录结构放置即可。

use deskpet::model::{make_model, PetState};
use deskpet::sprite_model::encode_png;

fn main() {
    let mut model = make_model(0); // 橘猫
    let root = std::path::Path::new("models/示例猫");
    let cdir = root.join("default");
    std::fs::create_dir_all(&cdir).unwrap();

    // 片段定义：(片段名, [(状态, tick)])
    let clips: Vec<(&str, Vec<(PetState, u32)>)> = vec![
        ("idle", vec![(PetState::Idle, 0), (PetState::Idle, 0), (PetState::Idle, 7), (PetState::Idle, 0)]),
        ("walk", vec![(PetState::Walk, 0), (PetState::Walk, 1)]),
        ("sleep", vec![(PetState::Sleep, 0)]),
        ("happy", vec![(PetState::Patted, 0)]),
        ("shock", vec![(PetState::Shocked, 0)]),
        ("dragged", vec![(PetState::Dragged, 0), (PetState::Dragged, 1)]),
        ("sit", vec![(PetState::Sitting, 0), (PetState::Sitting, 8)]),
        ("stretch", vec![(PetState::Stretch, 0)]),
        ("groom", vec![(PetState::Groom, 0), (PetState::Groom, 1)]),
        ("eat", vec![(PetState::Eat, 0), (PetState::Eat, 1)]),
        ("climb", vec![(PetState::Climb, 0), (PetState::Climb, 1)]),
        ("perch", vec![(PetState::Perch, 0), (PetState::Perch, 8)]),
    ];

    let mut total = 0;
    for (clip, frames) in &clips {
        for (i, (state, tick)) in frames.iter().enumerate() {
            let pose = deskpet::model::Pose {
                state: *state,
                tick: *tick,
                gaze: (0, 0),
                expr: 0,
                costume: 0,
                aux: 0,
                typing: false,
            };
            let argb = model.render(&pose);
            let path = cdir.join(format!("{clip}_{i}.png"));
            encode_png(&path, 64, 64, &argb).unwrap();
            total += 1;
        }
    }

    let toml = r#"name = "示例猫"
fps = 8
size = 64
expressions = ["自动", "开心", "惊讶", "困困", "星星眼", "脸红"]

# 一套换装 = 一个子目录（把整套帧复制一份改名换风格即可）
[[costume]]
name = "橘猫"
dir = "default"
"#;
    std::fs::write(root.join("model.toml"), toml).unwrap();
    println!("示例模型已导出：models/示例猫/（{total} 帧）");
    println!("自制模型：照 model.toml + <片段>_<帧号>.png 的结构放置即可");
}
