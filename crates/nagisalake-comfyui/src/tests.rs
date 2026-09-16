use crate::{
    config::ComfyUiConfig,
    parse::{parse_history, parse_queue_status},
    poll::build_service,
};
use nagisalake_core::{ComfyHistoryResponse, ComfyPromptStatus};
use serde_json::json;

/// ComfyUI's `/queue` entries are heterogeneous arrays shaped
/// `[number, prompt_id, prompt, extra_data, outputs]`, so the prompt id has
/// to be read positionally. Getting the index wrong would silently report
/// every prompt as unknown and the console would show no queue position.
#[test]
fn queue_status_distinguishes_running_from_queued() {
    let payload = json!({
        "queue_running": [[0, "running-prompt", {}, {}, []]],
        "queue_pending": [
            [1, "first-pending", {}, {}, []],
            [2, "second-pending", {}, {}, []]
        ]
    });

    assert_eq!(
        parse_queue_status("running-prompt", payload.clone()),
        ComfyPromptStatus::Running
    );
    // Position is 1-based so it reads naturally as "1st in line".
    assert_eq!(
        parse_queue_status("first-pending", payload.clone()),
        ComfyPromptStatus::Queued { position: 1 }
    );
    assert_eq!(
        parse_queue_status("second-pending", payload.clone()),
        ComfyPromptStatus::Queued { position: 2 }
    );
    // A prompt that has left the queue is Unknown, not Queued: the caller
    // keeps the last known status instead of resetting it.
    assert_eq!(
        parse_queue_status("finished-prompt", payload),
        ComfyPromptStatus::Unknown
    );
}

/// A malformed or empty payload must degrade to Unknown rather than panic;
/// this parses a response from an external process we do not control.
#[test]
fn queue_status_tolerates_unexpected_payloads() {
    for payload in [
        json!({}),
        json!({"queue_running": [], "queue_pending": []}),
        json!({"queue_running": "not-an-array"}),
        // Entry too short to hold a prompt id.
        json!({"queue_pending": [[0]]}),
        // Prompt id in the wrong position.
        json!({"queue_pending": [["wanted", 0]]}),
        json!({"queue_pending": [null]}),
        json!(null),
    ] {
        assert_eq!(
            parse_queue_status("wanted", payload.clone()),
            ComfyPromptStatus::Unknown,
            "payload {payload} should not yield a position"
        );
    }
}

#[test]
fn extracts_known_output_collections_only() {
    let history = json!({
        "prompt-1": {
            "status": {"completed": true},
            "outputs": {
                "1": {"images":[{"filename":"a.png","subfolder":"","type":"output"}]},
                "2": {"videos":[{"filename":"b.mp4","subfolder":"clips","type":"output"}]},
                "3": {"arbitrary":[{"filename":"secret.bin"}]}
            }
        }
    });
    let ComfyHistoryResponse::Complete(outputs) = parse_history("prompt-1", history).unwrap()
    else {
        panic!("expected completed history");
    };
    assert_eq!(outputs.len(), 2);
    assert_eq!(outputs[0].content_type, "image/png");
    assert_eq!(outputs[1].content_type, "video/mp4");
}

#[test]
fn stack_builds_without_contacting_comfyui() {
    build_service(ComfyUiConfig::default()).unwrap();
}
