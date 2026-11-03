//! Building (UI thread) and running (audio thread) a graph of [`AudioNode`]s.

use musiton_core::ids::ParamId;

use crate::node::{AudioIo, AudioNode, Event, ProcessContext};

/// Index of a node as returned by [`GraphBuilder::add`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeIdx(pub u32);

/// Errors when building a graph.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    /// A connection refers to a node that doesn't exist.
    #[error("unknown node {0:?}")]
    UnknownNode(NodeIdx),
    /// A connection refers to a channel the node doesn't have.
    #[error("node {0:?} has no channel {1}")]
    BadChannel(NodeIdx, usize),
    /// The connections form a feedback loop.
    #[error("routing contains a feedback loop")]
    Cycle,
    /// No output node set, or it has no output channels.
    #[error("graph has no output")]
    NoOutput,
}

#[derive(Debug, Clone, Copy)]
struct Edge {
    src: usize,
    src_ch: usize,
    dst: usize,
    dst_ch: usize,
}

/// Collects nodes and connections, then compiles them into a [`CompiledGraph`].
#[derive(Default)]
pub struct GraphBuilder {
    nodes: Vec<Box<dyn AudioNode>>,
    edges: Vec<Edge>,
    output: Option<NodeIdx>,
}

impl GraphBuilder {
    /// Empty builder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a node.
    pub fn add(&mut self, node: Box<dyn AudioNode>) -> NodeIdx {
        self.nodes.push(node);
        NodeIdx(self.nodes.len() as u32 - 1)
    }

    /// Feeds output channel `src_ch` of `src` into input `dst_ch` of `dst` (summed).
    pub fn connect(
        &mut self,
        src: NodeIdx,
        src_ch: usize,
        dst: NodeIdx,
        dst_ch: usize,
    ) -> Result<(), GraphError> {
        let s = self.node(src)?;
        if src_ch >= s.num_outputs() {
            return Err(GraphError::BadChannel(src, src_ch));
        }
        let d = self.node(dst)?;
        if dst_ch >= d.num_inputs() {
            return Err(GraphError::BadChannel(dst, dst_ch));
        }
        self.edges.push(Edge {
            src: src.0 as usize,
            src_ch,
            dst: dst.0 as usize,
            dst_ch,
        });
        Ok(())
    }

    /// The node whose outputs (channel 0 = left, 1 = right) go to the device.
    pub fn set_output(&mut self, node: NodeIdx) {
        self.output = Some(node);
    }

    fn node(&self, idx: NodeIdx) -> Result<&dyn AudioNode, GraphError> {
        self.nodes
            .get(idx.0 as usize)
            .map(|n| n.as_ref())
            .ok_or(GraphError::UnknownNode(idx))
    }

    /// Sorts the nodes topologically, prepares them and allocates all buffers.
    pub fn build(self, sample_rate: f32, max_block: usize) -> Result<CompiledGraph, GraphError> {
        let out_idx = self.output.ok_or(GraphError::NoOutput)?;
        if self.node(out_idx)?.num_outputs() == 0 {
            return Err(GraphError::NoOutput);
        }
        let n = self.nodes.len();
        let mut indegree = vec![0usize; n];
        for e in &self.edges {
            indegree[e.dst] += 1;
        }
        let mut ready: Vec<usize> = (0..n).filter(|&i| indegree[i] == 0).rev().collect();
        let mut order = Vec::with_capacity(n);
        while let Some(i) = ready.pop() {
            order.push(i);
            for e in self.edges.iter().filter(|e| e.src == i) {
                indegree[e.dst] -= 1;
                if indegree[e.dst] == 0 {
                    ready.push(e.dst);
                }
            }
        }
        if order.len() != n {
            return Err(GraphError::Cycle);
        }
        let mut slot_of = vec![0usize; n];
        for (slot, &node) in order.iter().enumerate() {
            slot_of[node] = slot;
        }

        let max_block = max_block.max(1);
        let mut nodes: Vec<Option<Box<dyn AudioNode>>> = self.nodes.into_iter().map(Some).collect();
        let mut slots = Vec::with_capacity(n);
        for &node_i in &order {
            let Some(mut node) = nodes[node_i].take() else {
                continue;
            };
            node.prepare(sample_rate, max_block);
            let feeds = self
                .edges
                .iter()
                .filter(|e| e.dst == node_i)
                .map(|e| Feed {
                    src: slot_of[e.src],
                    src_ch: e.src_ch,
                    dst_ch: e.dst_ch,
                })
                .collect();
            slots.push(Slot {
                inputs: vec![vec![0.0; max_block]; node.num_inputs()],
                outputs: vec![vec![0.0; max_block]; node.num_outputs()],
                node,
                feeds,
            });
        }
        let output = slot_of[out_idx.0 as usize];
        Ok(CompiledGraph {
            slots,
            slot_of,
            output,
            max_block,
        })
    }
}

struct Feed {
    src: usize,
    src_ch: usize,
    dst_ch: usize,
}

struct Slot {
    node: Box<dyn AudioNode>,
    inputs: Vec<Vec<f32>>,
    outputs: Vec<Vec<f32>>,
    feeds: Vec<Feed>,
}

/// A ready-to-run graph. Built off the audio thread, then sent to the engine.
pub struct CompiledGraph {
    slots: Vec<Slot>,
    slot_of: Vec<usize>,
    output: usize,
    max_block: usize,
}

impl CompiledGraph {
    /// Largest block this graph can process.
    pub fn max_block(&self) -> usize {
        self.max_block
    }

    /// Runs all nodes for `ctx.frames` frames (must be ≤ [`Self::max_block`]).
    pub fn process(&mut self, ctx: &ProcessContext) {
        let frames = ctx.frames.min(self.max_block);
        let events: &[Event] = &[];
        for i in 0..self.slots.len() {
            let (done, rest) = self.slots.split_at_mut(i);
            let Some(slot) = rest.first_mut() else {
                break;
            };
            for buf in &mut slot.inputs {
                if let Some(b) = buf.get_mut(..frames) {
                    b.fill(0.0);
                }
            }
            for f in &slot.feeds {
                let src = done
                    .get(f.src)
                    .and_then(|s| s.outputs.get(f.src_ch))
                    .and_then(|b| b.get(..frames));
                let dst = slot
                    .inputs
                    .get_mut(f.dst_ch)
                    .and_then(|b| b.get_mut(..frames));
                if let (Some(src), Some(dst)) = (src, dst) {
                    for (d, s) in dst.iter_mut().zip(src) {
                        *d += *s;
                    }
                }
            }
            let mut io = AudioIo::new(&slot.inputs, &mut slot.outputs, frames);
            slot.node.process(ctx, &mut io, events);
        }
    }

    /// Output channel `ch` of the output node after [`Self::process`] (empty if missing).
    pub fn output(&self, ch: usize, frames: usize) -> &[f32] {
        self.slots
            .get(self.output)
            .and_then(|s| s.outputs.get(ch))
            .and_then(|b| b.get(..frames))
            .unwrap_or(&[])
    }

    /// Number of output channels of the output node.
    pub fn num_outputs(&self) -> usize {
        self.slots.get(self.output).map_or(0, |s| s.outputs.len())
    }

    /// Sets a parameter on a node.
    pub fn set_param(&mut self, node: NodeIdx, id: ParamId, value: f32) {
        let slot = self.slot_of.get(node.0 as usize).copied();
        if let Some(s) = slot.and_then(|i| self.slots.get_mut(i)) {
            s.node.set_param(id, value);
        }
    }

    /// Gives `data` to a node; returns whatever must be freed (old data, or `data` itself
    /// if the node did not take it).
    pub fn send_data(
        &mut self,
        node: NodeIdx,
        data: Box<dyn std::any::Any + Send>,
    ) -> Option<Box<dyn std::any::Any + Send>> {
        let slot = self.slot_of.get(node.0 as usize).copied();
        match slot.and_then(|i| self.slots.get_mut(i)) {
            Some(s) => s.node.receive(data),
            None => Some(data),
        }
    }

    /// Resets all nodes.
    pub fn reset(&mut self) {
        for s in &mut self.slots {
            s.node.reset();
        }
    }
}
