use ort::session::Session;
use std::path::Path;

pub struct OnnxModel {
    pub session: Session,
}

impl OnnxModel {
    /// Load an ONNX model from the given path.
    pub fn load(path: &Path) -> Result<Self, ort::Error> {
        let session = Session::builder()?.commit_from_file(path)?;
        Ok(Self { session })
    }

    /// Return the names of the model's inputs.
    pub fn input_names(&self) -> Vec<&str> {
        self.session.inputs().iter().map(|i| i.name()).collect()
    }

    /// Return the names of the model's outputs.
    pub fn output_names(&self) -> Vec<&str> {
        self.session
            .outputs()
            .iter()
            .map(|o| o.name())
            .collect()
    }
}
