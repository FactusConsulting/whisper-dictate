use super::*;

#[test]
fn receiver_keeps_both_final_loss_counters_after_producers_are_dropped() {
    let (chunks, chunk_rx) = capture::audio_chunk_channel();
    let (events, mut rx) = pipeline_event_channel(1);
    rx.capture_overflow = Some(chunk_rx.overflow_metric());
    for index in 0..capture::CAPTURE_QUEUE_CAPACITY + 1 {
        chunks
            .try_send_latest(AudioChunk::Samples(vec![index as f32]))
            .unwrap();
    }
    for value in [1.0, 2.0, 3.0] {
        events
            .try_send_latest(PipelineEvent::Frame(vec![value]))
            .unwrap();
    }
    drop(chunks);
    drop(chunk_rx);
    drop(events);
    assert_eq!(
        rx.overflow_snapshot(),
        RawCaptureOverflow {
            capture_chunks: 1,
            pipeline_events: 2
        }
    );
    assert_eq!(rx.recv().unwrap(), PipelineEvent::Frame(vec![3.0]));
    assert!(rx.recv().is_err());
    assert_eq!(
        rx.overflow_snapshot(),
        RawCaptureOverflow {
            capture_chunks: 1,
            pipeline_events: 2
        }
    );
}
