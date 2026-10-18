//! External-process stages: a foreign program (a Python classifier, a GNU Radio
//! flowgraph, anything) joins a ZetaSDR graph by speaking [`wire`] on its stdio.

pub mod wire;
