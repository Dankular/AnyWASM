//! Lower Vulkan draw-parameter builtins to a WebGPU uniform without losing
//! D3D's SV_VertexID / SV_InstanceID base-offset subtraction.

use std::collections::{HashMap, HashSet};

const CAPABILITY: u16 = 17;
const EXTENSION: u16 = 10;
const ENTRY_POINT: u16 = 15;
const NAME: u16 = 5;
const TYPE_INT: u16 = 21;
const TYPE_FLOAT: u16 = 22;
const TYPE_VECTOR: u16 = 23;
const TYPE_STRUCT: u16 = 30;
const TYPE_POINTER: u16 = 32;
const CONSTANT: u16 = 43;
const VARIABLE: u16 = 59;
const LOAD: u16 = 61;
const STORE: u16 = 62;
const ACCESS_CHAIN: u16 = 65;
const DECORATE: u16 = 71;
const MEMBER_DECORATE: u16 = 72;
const FUNCTION: u16 = 54;

const DRAW_PARAMETERS: u32 = 4427;
const BASE_VERTEX: u32 = 4424;
const BASE_INSTANCE: u32 = 4425;
const DRAW_INDEX: u32 = 4426;
const INPUT: u32 = 1;
const UNIFORM: u32 = 2;
const OUTPUT: u32 = 3;
const BUILTIN: u32 = 11;
const POINT_SIZE: u32 = 1;
const BLOCK: u32 = 2;
const OFFSET: u32 = 35;
const BINDING: u32 = 33;
const DESCRIPTOR_SET: u32 = 34;

#[derive(Clone)]
struct Instruction {
    opcode: u16,
    operands: Vec<u32>,
}

impl Instruction {
    fn new(opcode: u16, operands: &[u32]) -> Self {
        Self {
            opcode,
            operands: operands.to_vec(),
        }
    }

    fn write(&self, output: &mut Vec<u32>) -> Result<(), String> {
        let count = self.operands.len() + 1;
        if count > u16::MAX as usize {
            return Err("SPIR-V instruction exceeds word-count limit".into());
        }
        output.push(((count as u32) << 16) | u32::from(self.opcode));
        output.extend_from_slice(&self.operands);
        Ok(())
    }
}

fn words(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|chunk| u32::from_le_bytes(chunk.try_into().unwrap()))
        .collect()
}

fn decode(input: &[u32]) -> Result<Vec<Instruction>, String> {
    let mut instructions = Vec::new();
    let mut cursor = 5;
    while cursor < input.len() {
        let count = (input[cursor] >> 16) as usize;
        let opcode = input[cursor] as u16;
        if count == 0
            || cursor
                .checked_add(count)
                .is_none_or(|end| end > input.len())
        {
            return Err("malformed SPIR-V instruction length".into());
        }
        instructions.push(Instruction::new(opcode, &input[cursor + 1..cursor + count]));
        if instructions.len() > 200_000 {
            return Err("SPIR-V instruction count exceeds limit".into());
        }
        cursor += count;
    }
    Ok(instructions)
}

fn string_end(words: &[u32]) -> Result<usize, String> {
    words
        .iter()
        .position(|word| word.to_le_bytes().contains(&0))
        .map(|position| position + 1)
        .ok_or_else(|| "unterminated SPIR-V string".into())
}

fn string_value(words: &[u32]) -> Result<String, String> {
    let mut bytes = Vec::new();
    for word in &words[..string_end(words)?] {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    bytes.truncate(bytes.iter().position(|byte| *byte == 0).unwrap());
    String::from_utf8(bytes).map_err(|_| "invalid UTF-8 SPIR-V extension name".into())
}

fn new_id(bound: &mut u32) -> Result<u32, String> {
    let id = *bound;
    *bound = bound
        .checked_add(1)
        .filter(|value| *value <= 1_000_000)
        .ok_or_else(|| "SPIR-V id bound exceeds limit".to_string())?;
    Ok(id)
}

/// Group 3, binding 0: one 16-byte `vec4<i32>` uniform.
/// x = BaseVertex, y = BaseInstance, z/w reserved. Guest draw indices remain
/// `VertexIndex - x` and `InstanceIndex - y` exactly as vkd3d emitted them.
pub(crate) fn lower(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() < 20 || bytes.len() % 4 != 0 {
        return Err("invalid SPIR-V word range".into());
    }
    let input = words(bytes);
    let mut bound = input[3];
    if input[0] != 0x0723_0203 || bound == 0 || bound > 999_000 {
        return Err("invalid SPIR-V header or id bound".into());
    }
    let instructions = decode(&input)?;
    let mut bases = HashMap::<u32, u32>::new();
    let mut point_sizes = HashSet::<u32>::new();
    let mut pointers = HashMap::<u32, (u32, u32)>::new();
    let mut variables = HashMap::<u32, (u32, u32)>::new();
    let mut signed_i32 = HashSet::<u32>::new();
    let mut float32 = HashSet::<u32>::new();
    let mut float_one = HashSet::<(u32, u32)>::new();
    let mut descriptor_sets = HashMap::<u32, u32>::new();
    let mut bindings = HashMap::<u32, u32>::new();
    let mut saw_draw_declaration = false;

    for instruction in &instructions {
        let a = &instruction.operands;
        match instruction.opcode {
            CAPABILITY if a.as_slice() == [DRAW_PARAMETERS] => saw_draw_declaration = true,
            EXTENSION if string_value(a)? == "SPV_KHR_shader_draw_parameters" => {
                saw_draw_declaration = true;
            }
            TYPE_INT if a.len() == 3 && a[1] == 32 && a[2] == 1 => {
                signed_i32.insert(a[0]);
            }
            TYPE_FLOAT if a.len() == 2 && a[1] == 32 => {
                float32.insert(a[0]);
            }
            CONSTANT if a.len() == 3 && a[2] == 0x3f80_0000 => {
                float_one.insert((a[0], a[1]));
            }
            TYPE_POINTER if a.len() == 3 => {
                pointers.insert(a[0], (a[1], a[2]));
            }
            VARIABLE if a.len() >= 3 => {
                variables.insert(a[1], (a[0], a[2]));
            }
            DECORATE if a.len() >= 3 && a[1] == BUILTIN => match a[2] {
                BASE_VERTEX | BASE_INSTANCE => {
                    if a.len() != 3 || bases.insert(a[0], a[2]).is_some() {
                        return Err("duplicate or malformed draw builtin decoration".into());
                    }
                }
                DRAW_INDEX => return Err("DrawIndex builtin is unsupported".into()),
                POINT_SIZE => {
                    if a.len() != 3 || !point_sizes.insert(a[0]) {
                        return Err("duplicate or malformed PointSize builtin".into());
                    }
                }
                _ => {}
            },
            MEMBER_DECORATE if a.len() >= 4 && a[2] == BUILTIN => {
                if matches!(a[3], BASE_VERTEX | BASE_INSTANCE | DRAW_INDEX) {
                    return Err("draw builtins in SPIR-V struct members are unsupported".into());
                }
            }
            DECORATE if a.len() == 3 && a[1] == DESCRIPTOR_SET => {
                descriptor_sets.insert(a[0], a[2]);
            }
            DECORATE if a.len() == 3 && a[1] == BINDING => {
                bindings.insert(a[0], a[2]);
            }
            _ => {}
        }
    }
    if bases.is_empty() && point_sizes.is_empty() {
        if saw_draw_declaration {
            return Err(
                "DrawParameters extension has no supported BaseVertex/BaseInstance input".into(),
            );
        }
        return Ok(bytes.to_vec());
    }
    for id in &point_sizes {
        let (pointer_type, storage) = variables
            .get(id)
            .copied()
            .ok_or_else(|| "PointSize does not decorate a variable".to_string())?;
        let (pointer_storage, pointee) = pointers
            .get(&pointer_type)
            .copied()
            .ok_or_else(|| "PointSize has no pointer type".to_string())?;
        if storage != OUTPUT || pointer_storage != OUTPUT || !float32.contains(&pointee) {
            return Err("PointSize must be a scalar f32 output".into());
        }
    }
    if bases.is_empty() {
        return Err("PointSize-only lowering is not implemented".into());
    }
    if descriptor_sets
        .iter()
        .any(|(id, set)| *set == 3 && bindings.get(id) == Some(&0))
    {
        return Err("draw-parameter uniform conflicts with descriptor set 3 binding 0".into());
    }
    let mut integer_type = None;
    for id in bases.keys() {
        let (pointer_type, storage) = variables
            .get(id)
            .copied()
            .ok_or_else(|| "draw builtin does not decorate a variable".to_string())?;
        let (pointer_storage, pointee) = pointers
            .get(&pointer_type)
            .copied()
            .ok_or_else(|| "draw builtin has no pointer type".to_string())?;
        if storage != INPUT || pointer_storage != INPUT || !signed_i32.contains(&pointee) {
            return Err("draw builtin must be a scalar signed i32 input".into());
        }
        if integer_type
            .replace(pointee)
            .is_some_and(|previous| previous != pointee)
        {
            return Err("draw builtins use different integer types".into());
        }
    }
    let i32_type = integer_type.unwrap();
    let vec4 = new_id(&mut bound)?;
    let block = new_id(&mut bound)?;
    let block_pointer = new_id(&mut bound)?;
    let scalar_pointer = new_id(&mut bound)?;
    let zero = new_id(&mut bound)?;
    let one = new_id(&mut bound)?;
    let uniform = new_id(&mut bound)?;
    let annotations = [
        Instruction::new(DECORATE, &[block, BLOCK]),
        Instruction::new(MEMBER_DECORATE, &[block, 0, OFFSET, 0]),
        Instruction::new(DECORATE, &[uniform, DESCRIPTOR_SET, 3]),
        Instruction::new(DECORATE, &[uniform, BINDING, 0]),
    ];
    let globals = [
        Instruction::new(TYPE_VECTOR, &[vec4, i32_type, 4]),
        Instruction::new(TYPE_STRUCT, &[block, vec4]),
        Instruction::new(TYPE_POINTER, &[block_pointer, UNIFORM, block]),
        Instruction::new(TYPE_POINTER, &[scalar_pointer, UNIFORM, i32_type]),
        Instruction::new(CONSTANT, &[i32_type, zero, 0]),
        Instruction::new(CONSTANT, &[i32_type, one, 1]),
        Instruction::new(VARIABLE, &[block_pointer, uniform, UNIFORM]),
    ];
    let first_type = instructions
        .iter()
        .position(|instruction| instruction.opcode == 19)
        .ok_or_else(|| "SPIR-V has no type declarations".to_string())?;
    let first_function = instructions
        .iter()
        .position(|instruction| instruction.opcode == FUNCTION)
        .ok_or_else(|| "SPIR-V has no function".to_string())?;
    if first_type >= first_function {
        return Err("SPIR-V type/function order is invalid".into());
    }

    let mut rewritten = input[..5].to_vec();
    let mut replaced_loads = HashSet::new();
    for (position, instruction) in instructions.iter().enumerate() {
        if position == first_type {
            for annotation in &annotations {
                annotation.write(&mut rewritten)?;
            }
        }
        if position == first_function {
            for global in &globals {
                global.write(&mut rewritten)?;
            }
        }
        let a = &instruction.operands;
        match instruction.opcode {
            CAPABILITY if a.as_slice() == [DRAW_PARAMETERS] => continue,
            EXTENSION if string_value(a)? == "SPV_KHR_shader_draw_parameters" => continue,
            DECORATE if a.first().is_some_and(|id| point_sizes.contains(id)) => {
                if a.len() != 3 || a[1] != BUILTIN || a[2] != POINT_SIZE {
                    return Err("PointSize carries an unsupported decoration".into());
                }
                continue;
            }
            DECORATE if a.first().is_some_and(|id| bases.contains_key(id)) => {
                if a.len() != 3 || a[1] != BUILTIN || bases.get(&a[0]) != Some(&a[2]) {
                    return Err("draw input carries an unsupported decoration".into());
                }
                continue;
            }
            NAME if a
                .first()
                .is_some_and(|id| bases.contains_key(id) || point_sizes.contains(id)) =>
            {
                continue
            }
            VARIABLE
                if a.len() >= 3 && (bases.contains_key(&a[1]) || point_sizes.contains(&a[1])) =>
            {
                continue
            }
            ENTRY_POINT => {
                if a.len() < 3 {
                    return Err("malformed SPIR-V entry point".into());
                }
                let interfaces = 2 + string_end(&a[2..])?;
                let mut retained = a[..interfaces].to_vec();
                retained.extend(
                    a[interfaces..]
                        .iter()
                        .copied()
                        .filter(|id| !bases.contains_key(id) && !point_sizes.contains(id)),
                );
                Instruction::new(ENTRY_POINT, &retained).write(&mut rewritten)?;
                continue;
            }
            LOAD if a.len() >= 3 && bases.contains_key(&a[2]) => {
                if position < first_function || a[0] != i32_type || a.len() != 3 {
                    return Err("draw builtin load form is unsupported".into());
                }
                let component = if bases[&a[2]] == BASE_VERTEX {
                    zero
                } else {
                    one
                };
                let access = new_id(&mut bound)?;
                Instruction::new(
                    ACCESS_CHAIN,
                    &[scalar_pointer, access, uniform, zero, component],
                )
                .write(&mut rewritten)?;
                Instruction::new(LOAD, &[a[0], a[1], access]).write(&mut rewritten)?;
                replaced_loads.insert(a[2]);
                continue;
            }
            STORE if a.len() == 2 && point_sizes.contains(&a[0]) => {
                let (_, pointee) = pointers[&variables[&a[0]].0];
                if !float_one.contains(&(pointee, a[1])) {
                    return Err("only fixed 1.0 PointSize output is supported".into());
                }
                continue;
            }
            _ => {}
        }
        if position >= first_function
            && instruction.opcode != 8 // OpLine operands are file/line/column literals.
            && a.iter()
                .any(|value| bases.contains_key(value) || point_sizes.contains(value))
        {
            return Err(format!("draw builtin is used outside a supported direct load or store at opcode {} operands {:?}", instruction.opcode, a));
        }
        instruction.write(&mut rewritten)?;
    }
    if replaced_loads.len() != bases.len() {
        return Err("draw builtin has no supported load".into());
    }
    rewritten[3] = bound;
    let mut output = Vec::with_capacity(rewritten.len() * 4);
    for word in rewritten {
        output.extend_from_slice(&word.to_le_bytes());
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(instructions: &[Instruction]) -> Vec<u8> {
        let mut words = vec![0x0723_0203, 0x0001_0000, 0, 20, 0];
        for instruction in instructions {
            instruction.write(&mut words).unwrap();
        }
        words.into_iter().flat_map(u32::to_le_bytes).collect()
    }

    #[test]
    fn rejects_truncated_instruction_and_unsupported_draw_index() {
        let mut truncated = module(&[Instruction::new(CAPABILITY, &[1])]);
        truncated.truncate(truncated.len() - 4);
        assert!(lower(&truncated)
            .unwrap_err()
            .contains("instruction length"));
        let unsupported = module(&[Instruction::new(DECORATE, &[7, BUILTIN, DRAW_INDEX])]);
        assert!(lower(&unsupported).unwrap_err().contains("DrawIndex"));
    }

    #[test]
    fn rejects_base_input_without_variable_and_unused_extension() {
        let missing = module(&[Instruction::new(DECORATE, &[7, BUILTIN, BASE_VERTEX])]);
        assert!(lower(&missing)
            .unwrap_err()
            .contains("does not decorate a variable"));
        let unused = module(&[Instruction::new(CAPABILITY, &[DRAW_PARAMETERS])]);
        assert!(lower(&unused).unwrap_err().contains("no supported"));
    }
}
