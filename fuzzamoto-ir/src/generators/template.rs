use rand::{Rng, RngCore};

use crate::{
    Generator, GeneratorResult, Instruction, Operation, PerTestcaseMetadata, ProgramBuilder
};

/// `TemplateGenerator` inserts `template` operation in response to the `gettemplate` message
#[derive(Default)]
pub struct TemplateGenerator;

impl<R: RngCore> Generator<R> for TemplateGenerator {
    fn generate(
        &self,
        builder: &mut ProgramBuilder,
        rng: &mut R,
        meta: Option<&PerTestcaseMetadata>,
    ) -> GeneratorResult {
        if let Some(meta) = meta
            && !meta.template_request().is_empty()
        {
            let insertion_point = builder.instructions.len();
            assert_eq!(
                builder
                    .instructions
                    .get(insertion_point - 1)
                    .expect("The insertion point should exist")
                    .operation,
                Operation::SendGetTemplate
            );
            let template_req = meta.template_request();
            let choice = template_req
                .iter()
                .position(|x| x.triggering_instruction_index == insertion_point - 1)
                .expect("Triggering instruction not found");

            let template = builder
                .append(Instruction {
                    inputs: vec![mut_block_txn.index],
                    operation: Operation::BuildTemplate,
                })
                .expect("Inserting BuildTemplate should always succeed")
                .pop()
                .expect("BuildTemplate should always produce a var");

            let connection = template_req[choice].connection_index;
            builder
                .append(Instruction {
                    inputs: vec![connection, block_txn.index],
                    operation: Operation::SendTemplate,
                })
                .expect("Inserting SendTemplate should always succeed");
        }

        Ok(())
    }

    fn name(&self) -> &'static str {
        "TemplateGenerator"
    }

    fn choose_index(
        &self,
        program: &crate::Program,
        rng: &mut R,
        meta: Option<&PerTestcaseMetadata>,
    ) -> Option<usize> {
        if let Some(meta) = meta
            && !meta.block_txn_request().is_empty()
        {
            let blocktxn_req = meta.block_txn_request();
            let choice = rng.gen_range(0..blocktxn_req.len());
            let insertion_point = blocktxn_req[choice].triggering_instruction_index + 1;
            Some(insertion_point)
        } else {
            program
                .get_random_instruction_index(rng, <Self as Generator<R>>::requested_context(self))
        }
    }
}

impl TemplateGenerator {
    pub fn new() -> Self {
        Self {}
    }
}
