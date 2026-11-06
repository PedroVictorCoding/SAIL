//! Emit actual Dialogue prompt/tool requests for local before/after measurements.
//! cargo run -p sail-dialogue --example voice_prompt_cases -- /tmp/baseline-system.txt
use sail_dialogue::{prompt, vlm};
use serde_json::json;
fn main() {
    let baseline = std::env::args()
        .nth(1)
        .map(|p| std::fs::read_to_string(p).expect("baseline system file"));
    let cases = [
        (
            "short_answer",
            "What is 17 multiplied by 6? Answer briefly.",
        ),
        ("workspace", "List the files in the workspace root."),
        ("reminder", "Remind me later to check the bicycle."),
        ("capabilities", "What tools do you have?"),
    ];
    let requests: Vec<_> = cases
        .into_iter()
        .map(|(name, user)| {
            let memory = prompt::clock_context("2026-11-05T12:00:00Z", "Europe/Lisbon", 0);
            let messages = json!([
                {"role":"system","content":prompt::system_prompt("/home/main/SAIL")},
                {"role":"user","content":vlm::with_memory_context(user, &memory)}
            ]);
            let tools = if name == "capabilities" {
                json!([])
            } else {
                vlm::selected_tool_definitions(messages.as_array().unwrap())
            };
            let mut before = messages.clone();
            if let Some(system) = &baseline {
                before[0]["content"] = json!(system);
                before[1]["content"] = json!(user);
            }
            json!({"name":name,"before":before,"after":messages,"tools":tools})
        })
        .collect();
    println!("{}", json!(requests));
}
