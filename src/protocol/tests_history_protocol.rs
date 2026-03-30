//! Protocol round-trip tests for history and screen update messages.
//!
//! These tests were originally in screen/history_boundary_tests.rs but moved here
//! because they depend on the protocol module (binary crate only), while the screen
//! module now lives in the library crate.

use crate::protocol::{self, ServerMsg};

#[test]
fn history_message_round_trip() {
    let lines = vec![
        b"line one".to_vec(),
        b"line two with \x1b[1mbold\x1b[0m".to_vec(),
        b"line three".to_vec(),
    ];
    let msg = ServerMsg::History(lines.clone());
    let encoded = protocol::encode(&msg).unwrap();

    let (data, consumed) = protocol::codec::decode_frame(&encoded).unwrap().unwrap();
    assert_eq!(consumed, encoded.len());

    let decoded: ServerMsg = protocol::codec::decode(data).unwrap();
    match decoded {
        ServerMsg::History(decoded_lines) => {
            assert_eq!(decoded_lines.len(), 3);
            assert_eq!(decoded_lines[0], b"line one");
            assert_eq!(decoded_lines[2], b"line three");
        }
        other => panic!("expected History, got {:?}", other),
    }
}

#[test]
fn screen_update_message_round_trip() {
    let update_data = b"\x1b[?2026h\x1b[?25l\x1b[2J\x1b[HHello\x1b[?2026l".to_vec();
    let msg = ServerMsg::ScreenUpdate(update_data.clone());
    let encoded = protocol::encode(&msg).unwrap();

    let (data, _) = protocol::codec::decode_frame(&encoded).unwrap().unwrap();
    let decoded: ServerMsg = protocol::codec::decode(data).unwrap();
    match decoded {
        ServerMsg::ScreenUpdate(decoded_data) => {
            assert_eq!(decoded_data, update_data);
        }
        other => panic!("expected ScreenUpdate, got {:?}", other),
    }
}

#[test]
fn history_chunking_round_trip() {
    // Simulate the chunking logic from send_initial_state
    let mut all_lines = Vec::new();
    for i in 0..500 {
        all_lines.push(format!("history line {:04}", i).into_bytes());
    }

    let size_limit = protocol::codec::MAX_FRAME_SIZE / 2;
    let mut chunks: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut chunk = Vec::new();
    let mut chunk_size = 0;

    for line in &all_lines {
        let line_size = line.len() + 16;
        if chunk_size + line_size > size_limit && !chunk.is_empty() {
            chunks.push(std::mem::take(&mut chunk));
            chunk_size = 0;
        }
        chunk_size += line_size;
        chunk.push(line.clone());
    }
    if !chunk.is_empty() {
        chunks.push(chunk);
    }

    // Encode all chunks, then decode and reassemble
    let mut reassembled = Vec::new();
    for chunk_lines in &chunks {
        let msg = ServerMsg::History(chunk_lines.clone());
        let encoded = protocol::encode(&msg).unwrap();
        let (data, _) = protocol::codec::decode_frame(&encoded).unwrap().unwrap();
        let decoded: ServerMsg = protocol::codec::decode(data).unwrap();
        match decoded {
            ServerMsg::History(lines) => reassembled.extend(lines),
            other => panic!("expected History, got {:?}", other),
        }
    }

    assert_eq!(reassembled.len(), all_lines.len());
    for (i, line) in reassembled.iter().enumerate() {
        assert_eq!(
            line, &all_lines[i],
            "line {} mismatch after chunked round-trip",
            i
        );
    }
}

#[test]
fn e2e_reattach_protocol_encode_decode_sequence() {
    use retach::screen::{Screen, RenderCache};
    use retach::screen::write_u16;

    let mut screen = Screen::new(10, 3, 100);
    // Write 6 labeled lines
    for i in 1..=6 {
        if i < 6 {
            screen.process(format!("L{:02}\r\n", i).as_bytes());
        } else {
            screen.process(format!("L{:02}", i).as_bytes());
        }
    }
    let _ = screen.take_pending_scrollback();

    // Simulate reattach
    let hist = screen.get_history();
    let mut render_data = Vec::new();
    if !hist.is_empty() {
        render_data.extend_from_slice(b"\x1b[");
        write_u16(&mut render_data, screen.rows());
        render_data.extend_from_slice(b";1H");
        render_data.extend(
            std::iter::repeat_n(b'\n', screen.rows().saturating_sub(1) as usize),
        );
    }
    let mut cache = RenderCache::new();
    render_data.extend_from_slice(&screen.render(true, &mut cache));

    // Encode the full message sequence as the server would
    let mut wire = Vec::new();
    wire.extend(
        protocol::encode(&ServerMsg::Connected {
            name: "test".into(),
            new_session: false,
        })
        .unwrap(),
    );
    if !hist.is_empty() {
        wire.extend(protocol::encode(&ServerMsg::History(hist)).unwrap());
    }
    wire.extend(protocol::encode(&ServerMsg::ScreenUpdate(render_data)).unwrap());

    // Decode the sequence as the client would
    let mut offset = 0;
    let mut messages = Vec::new();
    while offset < wire.len() {
        let (data, consumed) = protocol::codec::decode_frame(&wire[offset..])
            .unwrap()
            .expect("should decode complete frame");
        let msg: ServerMsg = protocol::codec::decode(data).unwrap();
        messages.push(msg);
        offset += consumed;
    }

    // Verify message order: Connected -> History -> ScreenUpdate
    assert!(matches!(messages[0], ServerMsg::Connected { .. }));
    assert!(matches!(messages[1], ServerMsg::History(_)));
    assert!(matches!(messages[2], ServerMsg::ScreenUpdate(_)));

    // Simulate client stdout
    let mut stdout = Vec::new();
    for msg in &messages {
        match msg {
            ServerMsg::History(lines) => {
                for line in lines {
                    stdout.extend_from_slice(line);
                    stdout.extend_from_slice(b"\r\n");
                }
            }
            ServerMsg::ScreenUpdate(data) => {
                stdout.extend_from_slice(data);
            }
            _ => {}
        }
    }

    let text = String::from_utf8_lossy(&stdout);
    // History lines present
    assert!(text.contains("L01"), "L01 should be in output");
    assert!(text.contains("L03"), "L03 should be in output");
    // Screen content present after screen clear
    let pos_clear = text.find("\x1b[2J").expect("screen clear");
    let after_clear = &text[pos_clear..];
    assert!(after_clear.contains("L04"), "L04 should be on screen");
    assert!(after_clear.contains("L06"), "L06 should be on screen");
}
