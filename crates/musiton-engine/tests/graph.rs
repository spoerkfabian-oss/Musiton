//! Graph building and routing.

mod common;

use common::{Const, Gain};
use musiton_core::ids::ParamId;
use musiton_core::TempoMap;
use musiton_engine::graph::{GraphBuilder, GraphError, NodeIdx};
use musiton_engine::node::ProcessContext;

fn ctx(tempo: &TempoMap, frames: usize) -> ProcessContext<'_> {
    ProcessContext {
        sample_rate: 48_000.0,
        frames,
        playing: false,
        sample_pos: 0,
        tempo,
        time_sig: Default::default(),
        metronome: false,
    }
}

#[test]
fn inputs_are_summed_in_topological_order() {
    let mut b = GraphBuilder::new();
    // Added out of order on purpose: the sink comes first.
    let sink = b.add(Box::new(Gain {
        gain: 0.5,
        channels: 1,
    }));
    let a = b.add(Box::new(Const {
        value: 1.0,
        channels: 1,
    }));
    let c = b.add(Box::new(Const {
        value: 2.0,
        channels: 1,
    }));
    b.connect(a, 0, sink, 0).unwrap();
    b.connect(c, 0, sink, 0).unwrap();
    b.set_output(sink);
    let mut g = b.build(48_000.0, 64).unwrap();
    let tempo = TempoMap::default();
    g.process(&ctx(&tempo, 64));
    assert!(g.output(0, 64).iter().all(|&s| s == 1.5));
    assert_eq!(g.output(0, 64).len(), 64);

    g.set_param(sink, ParamId(0), 2.0);
    g.process(&ctx(&tempo, 32));
    assert!(g.output(0, 32).iter().all(|&s| s == 6.0));
}

#[test]
fn cycles_are_rejected() {
    let mut b = GraphBuilder::new();
    let x = b.add(Box::new(Gain {
        gain: 1.0,
        channels: 1,
    }));
    let y = b.add(Box::new(Gain {
        gain: 1.0,
        channels: 1,
    }));
    b.connect(x, 0, y, 0).unwrap();
    b.connect(y, 0, x, 0).unwrap();
    b.set_output(y);
    assert_eq!(b.build(48_000.0, 64).err(), Some(GraphError::Cycle));
}

#[test]
fn bad_connections_are_rejected() {
    let mut b = GraphBuilder::new();
    let src = b.add(Box::new(Const {
        value: 1.0,
        channels: 1,
    }));
    let dst = b.add(Box::new(Gain {
        gain: 1.0,
        channels: 2,
    }));
    assert_eq!(
        b.connect(src, 1, dst, 0),
        Err(GraphError::BadChannel(src, 1))
    );
    assert_eq!(
        b.connect(src, 0, dst, 2),
        Err(GraphError::BadChannel(dst, 2))
    );
    assert_eq!(
        b.connect(src, 0, NodeIdx(9), 0),
        Err(GraphError::UnknownNode(NodeIdx(9)))
    );
    assert_eq!(b.build(48_000.0, 64).err(), Some(GraphError::NoOutput));
}
