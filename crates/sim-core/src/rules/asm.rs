//! A tiny assembler for [`Op`] programs: labels with forward references,
//! one method per instruction. The compiler's back end, and how the
//! built-in programs are written until the compiler exists.

use super::vm::{Action, Op, OpCode, Sense};

/// A forward-referenceable code position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Label(usize);

/// Builds one `Vec<Op>`. Jumps are relative to the instruction after the
/// jump; labels resolve when [`Asm::finish`] runs.
#[derive(Debug, Default)]
pub struct Asm {
    code: Vec<Op>,
    labels: Vec<Option<usize>>,
    /// `(instruction index, label)` to patch.
    fixups: Vec<(usize, Label)>,
}

impl Asm {
    pub fn new() -> Self {
        Self::default()
    }

    /// Index of the next instruction.
    pub fn here(&self) -> u32 {
        u32::try_from(self.code.len()).expect("program fits u32")
    }

    pub fn label(&mut self) -> Label {
        self.labels.push(None);
        Label(self.labels.len() - 1)
    }

    /// Place `l` at the next instruction.
    pub fn bind(&mut self, l: Label) -> &mut Self {
        assert!(self.labels[l.0].is_none(), "label bound twice");
        self.labels[l.0] = Some(self.code.len());
        self
    }

    pub fn op(&mut self, code: OpCode) -> &mut Self {
        self.code.push(Op::new(code, 0, 0));
        self
    }

    fn emit(&mut self, code: OpCode, a: u8, imm: i16) -> &mut Self {
        self.code.push(Op::new(code, a, imm));
        self
    }

    fn jump(&mut self, code: OpCode, l: Label) -> &mut Self {
        self.fixups.push((self.code.len(), l));
        self.emit(code, 0, 0)
    }

    pub fn push(&mut self, v: i32) -> &mut Self {
        let imm = i16::try_from(v).expect("push immediate fits i16; use push_k");
        self.emit(OpCode::Push, 0, imm)
    }

    pub fn push_k(&mut self, index: u16) -> &mut Self {
        self.emit(OpCode::PushK, 0, index as i16)
    }

    pub fn load(&mut self, slot: u8) -> &mut Self {
        self.emit(OpCode::Load, slot, 0)
    }

    pub fn store(&mut self, slot: u8) -> &mut Self {
        self.emit(OpCode::Store, slot, 0)
    }

    pub fn need(&mut self, i: u8) -> &mut Self {
        self.emit(OpCode::Need, i, 0)
    }

    pub fn set_need(&mut self, i: u8) -> &mut Self {
        self.emit(OpCode::SetNeed, i, 0)
    }

    pub fn mem(&mut self, i: u8) -> &mut Self {
        self.emit(OpCode::Mem, i, 0)
    }

    pub fn set_mem(&mut self, i: u8) -> &mut Self {
        self.emit(OpCode::SetMem, i, 0)
    }

    pub fn sense(&mut self, s: Sense) -> &mut Self {
        self.emit(OpCode::Sense, s as u8, 0)
    }

    pub fn jmp(&mut self, l: Label) -> &mut Self {
        self.jump(OpCode::Jmp, l)
    }

    pub fn jz(&mut self, l: Label) -> &mut Self {
        self.jump(OpCode::Jz, l)
    }

    pub fn jnz(&mut self, l: Label) -> &mut Self {
        self.jump(OpCode::Jnz, l)
    }

    /// A raw relative jump, for tests of bad targets.
    pub fn jmp_raw(&mut self, imm: i16) -> &mut Self {
        self.emit(OpCode::Jmp, 0, imm)
    }

    /// Call `subs[sub]` with the top `args` stack values as its locals.
    pub fn call(&mut self, sub: u16, args: u8) -> &mut Self {
        self.emit(OpCode::Call, args, sub as i16)
    }

    pub fn ret(&mut self, value: bool) -> &mut Self {
        self.emit(OpCode::Ret, u8::from(value), 0)
    }

    /// `pred r -> found`, binding `(dx, dy)` into locals `slot`, `slot + 1`.
    pub fn nearest(&mut self, slot: u8) -> &mut Self {
        self.emit(OpCode::Nearest, slot, 0)
    }

    /// `ch r -> found`, binding the strongest scent cell into `slot`, `slot + 1`.
    pub fn sniff(&mut self, slot: u8) -> &mut Self {
        self.emit(OpCode::Sniff, slot, 0)
    }

    /// `v ->`: mark scent channel `ch` on the actor's cell.
    pub fn mark(&mut self, ch: u8) -> &mut Self {
        self.emit(OpCode::Mark, ch, 0)
    }

    /// `dx dy -> v`: scent channel `ch` there.
    pub fn scent_at(&mut self, ch: u8) -> &mut Self {
        self.emit(OpCode::ScentAt, ch, 0)
    }

    /// One `for each` step over locals `slot..slot + 5` (`-> found`).
    pub fn for_each(&mut self, slot: u8) -> &mut Self {
        self.emit(OpCode::ForEach, slot, 0)
    }

    pub fn act(&mut self, a: Action) -> &mut Self {
        self.emit(OpCode::Act, a as u8, 0)
    }

    pub fn next(&mut self, state: u8) -> &mut Self {
        self.emit(OpCode::Next, state, 0)
    }

    pub fn end_rule(&mut self) -> &mut Self {
        self.op(OpCode::EndRule)
    }

    pub fn halt(&mut self) -> &mut Self {
        self.op(OpCode::Halt)
    }

    /// Resolve labels and hand over the code. Panics on an unbound label
    /// or a jump too far for 16 bits (a program bug, not a run-time one).
    pub fn finish(mut self) -> Vec<Op> {
        for (at, l) in self.fixups.drain(..) {
            let target = self.labels[l.0].expect("label bound");
            let rel = target as i64 - (at as i64 + 1);
            self.code[at].imm = i16::try_from(rel).expect("jump within 16 bits");
        }
        self.code
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_resolve_forward_and_backward() {
        let mut a = Asm::new();
        let top = a.label();
        let end = a.label();
        a.bind(top);
        a.push(1).jz(end).push(2).jmp(top).bind(end).halt();
        let code = a.finish();
        assert_eq!(code[1], Op::new(OpCode::Jz, 0, 2)); // to index 4 from index 2
        assert_eq!(code[3], Op::new(OpCode::Jmp, 0, -4)); // to index 0 from index 4
        assert_eq!(code[4].code, OpCode::Halt);
        assert_eq!(code[0].bits(), 1 << 16); // Push, a = 0, imm = 1
    }

    #[test]
    #[should_panic(expected = "label bound")]
    fn unbound_label_panics() {
        let mut a = Asm::new();
        let l = a.label();
        a.jz(l);
        a.finish();
    }
}
