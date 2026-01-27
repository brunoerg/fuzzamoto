use rand::RngCore;

use crate::{
    Operation, PerTestcaseMetadata,
    generators::{Generator, GeneratorError, GeneratorResult, ProgramBuilder},
};

#[derive(Default)]
pub struct GetTemplateGenerator;

impl<R: RngCore> Generator<R> for GetTemplateGenerator {
    fn generate(
        &self,
        builder: &mut ProgramBuilder,
        rng: &mut R,
        _meta: Option<&PerTestcaseMetadata>,
    ) -> GeneratorResult {
        if builder.context().num_connections == 0 {
            return Err(GeneratorError::InvalidContext(builder.context().clone()));
        }

        let conn_var = builder.get_or_create_random_connection(rng);
        builder.force_append(vec![conn_var.index], Operation::SendGetTemplate);

        Ok(())
    }

    fn name(&self) -> &'static str {
        "GetTemplateGenerator"
    }
}
