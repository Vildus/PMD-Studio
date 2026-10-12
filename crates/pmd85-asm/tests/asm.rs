//! Hand-written programs covering the assembler's features.

use std::collections::BTreeMap;

use pmd85_asm::{assemble, AsmOutput, SourceResolver};

/// A resolver over in-memory files.
struct Mem {
    text: BTreeMap<String, String>,
    bytes: BTreeMap<String, Vec<u8>>,
}

impl Mem {
    fn new(src: &str) -> Self {
        Mem {
            text: [("main.asm".to_string(), src.to_string())]
                .into_iter()
                .collect(),
            bytes: BTreeMap::new(),
        }
    }

    fn with_files(text: BTreeMap<String, String>, bytes: BTreeMap<String, Vec<u8>>) -> Self {
        Mem { text, bytes }
    }
}

impl SourceResolver for Mem {
    fn read_text(&mut self, path: &str) -> Result<String, String> {
        self.text
            .get(path)
            .cloned()
            .ok_or_else(|| format!("no such file {path:?}"))
    }

    fn read_bytes(&mut self, path: &str) -> Result<Vec<u8>, String> {
        self.bytes
            .get(path)
            .cloned()
            .ok_or_else(|| format!("no such file {path:?}"))
    }
}

fn asm(src: &str) -> AsmOutput {
    let mut mem = Mem::new(src);
    let out = assemble("main.asm", &mut mem);
    assert!(
        out.diagnostics.is_empty(),
        "unexpected diagnostics: {:#?}",
        out.diagnostics
    );
    out
}

fn image(src: &str) -> Vec<u8> {
    asm(src).flat_image(0xFF).unwrap().1
}

#[test]
fn hello_world_encoding() {
    // The canonical "print a message" program, monitor style.
    let src = r#"
        ORG 1000H
START:  LXI H,MSG
LOOP:   MOV A,M
        ORA A
        RZ
        CALL 0EDC4H   ; the monitor's tape block reader (any word here)
        INX H
        JMP LOOP
MSG:    DB "HELLO, PMD!",0
        END START
"#;
    let out = asm(src);
    assert_eq!(out.entry, Some(0x1000));
    assert_eq!(out.symbols.get("START"), Some(&0x1000));
    assert_eq!(out.symbols.get("MSG"), Some(&0x100D));
    assert_eq!(out.symbols.get("LOOP"), Some(&0x1003));
    let bytes = image(src);
    assert_eq!(
        bytes,
        vec![
            0x21, 0x0D, 0x10, // LXI H,100D
            0x7E, // MOV A,M
            0xB7, // ORA A
            0xC8, // RZ
            0xCD, 0xC4, 0xED, // CALL 0EDC4
            0x23, // INX H
            0xC3, 0x03, 0x10, // JMP 1003
            b'H', b'E', b'L', b'L', b'O', b',', b' ', b'P', b'M', b'D', b'!', 0,
        ]
    );
}

#[test]
fn labels_case_insensitive_and_forward() {
    let bytes = image("jnz NEXT\nNOP\nNEXT: mvi a,TOUCH\n hlt\nTOUCH EQU 7");
    assert_eq!(bytes, vec![0xC2, 0x04, 0x00, 0x00, 0x3E, 0x07, 0x76]);
}

#[test]
fn expressions_and_precedence() {
    // (1+2)*3 = 9; 0x10<<1 | 1 = 0x21; -2 & 0xFF = 0xFE.
    let bytes = image("DB (1+2)*3, 10H<<1|1, -2 & 0FFH, 'A'+2, 6/2, 7%3, ~0 & 0FFH, 1<<0");
    assert_eq!(bytes, vec![9, 0x21, 0xFE, b'C', 3, 1, 0xFF, 1]);
}

#[test]
fn dollar_is_current_address() {
    // JMP $ jumps to itself; HERE is the MVI's address.
    let src = "ORG 10H\nJMP $\nHERE EQU $\nMVI A,1";
    let out = asm(src);
    assert_eq!(out.symbols.get("HERE"), Some(&0x13));
    assert_eq!(image(src), vec![0xC3, 0x10, 0x00, 0x3E, 0x01]);
}

#[test]
fn local_labels_scope_to_the_enclosing_global() {
    let src = r#"
        ORG 0
TOP:    JMP .next
.next:  MVI A,1
BOT:    JMP .next
.next:  MVI A,2
"#;
    let out = asm(src);
    assert_eq!(out.symbols.get("TOP.NEXT"), Some(&0x0003));
    assert_eq!(out.symbols.get("BOT.NEXT"), Some(&0x0008));
    assert_eq!(
        image(src),
        vec![
            0xC3, 0x03, 0x00, // TOP: JMP TOP.next
            0x3E, 0x01, 0xC3, 0x08, 0x00, // BOT: JMP BOT.next
            0x3E, 0x02,
        ]
    );
}

#[test]
fn equ_fixpoint_any_order() {
    let src = "A EQU B+1\nB EQU C\nC EQU 5\nMVI A,A\nMVI B,B";
    assert_eq!(image(src), vec![0x3E, 0x06, 0x06, 0x05]);
}

#[test]
fn org_makes_segments_and_gaps_fill() {
    let src = "ORG 100H\n NOP\nORG 200H\n HLT";
    let out = asm(src);
    assert_eq!(out.segments.len(), 2);
    let (base, image) = out.flat_image(0xAA).unwrap();
    assert_eq!(base, 0x100);
    assert_eq!(image.len(), 0x101);
    assert_eq!(image[0], 0x00);
    assert_eq!(image[1], 0xAA);
    assert_eq!(*image.last().unwrap(), 0x76);
}

#[test]
fn ds_align_dw() {
    let src = "ORG 0\nDB 1\nALIGN 8\nDW 1234H, 0FF00H\nDS 3,0FFH\nNOP";
    let out = asm(src);
    let (_, image) = out.flat_image(0).unwrap();
    assert_eq!(
        image,
        vec![
            0x01, 0, 0, 0, 0, 0, 0, 0, // DB 1 + ALIGN 8
            0x34, 0x12, 0x00, 0xFF, // DW
            0xFF, 0xFF, 0xFF, // DS 3
            0x00, // NOP
        ]
    );
}

#[test]
fn include_splices_and_incbin_reads() {
    let mut text = BTreeMap::new();
    text.insert(
        "main.asm".to_string(),
        "INCLUDE \"lib.asm\"\nINCBIN \"data.bin\"\nDB 9".to_string(),
    );
    text.insert(
        "lib.asm".to_string(),
        "NOP\nINCLUDE \"sub.asm\"\nHLT".to_string(),
    );
    text.insert("sub.asm".to_string(), "MVI A,1".to_string());
    let mut bytes = BTreeMap::new();
    bytes.insert("data.bin".to_string(), vec![0xDE, 0xAD]);
    let mut mem = Mem::with_files(text, bytes);
    let out = assemble("main.asm", &mut mem);
    assert!(out.diagnostics.is_empty(), "{:#?}", out.diagnostics);
    assert_eq!(out.files.len(), 3);
    let (_, image) = out.flat_image(0).unwrap();
    assert_eq!(image, vec![0x00, 0x3E, 0x01, 0x76, 0xDE, 0xAD, 0x09]);
}

#[test]
fn incbin_offset_and_length() {
    let mut text = BTreeMap::new();
    text.insert(
        "main.asm".to_string(),
        "INCBIN \"data.bin\", 2, 3".to_string(),
    );
    let mut bytes = BTreeMap::new();
    bytes.insert("data.bin".to_string(), vec![1, 2, 3, 4, 5, 6]);
    let mut mem = Mem::with_files(text, bytes);
    let out = assemble("main.asm", &mut mem);
    assert!(out.diagnostics.is_empty(), "{:#?}", out.diagnostics);
    let (_, image) = out.flat_image(0).unwrap();
    assert_eq!(image, vec![3, 4, 5]);
}

#[test]
fn listing_and_address_lookup() {
    let src = "NOP\nMSG: DB \"AB\"\nHLT";
    let out = asm(src);
    assert_eq!(out.listing.len(), 3);
    assert_eq!(out.listing[0].addr, 0);
    assert_eq!(out.listing[0].bytes, vec![0x00]);
    assert_eq!(out.listing[1].addr, 1);
    assert_eq!(out.listing[1].bytes, vec![b'A', b'B']);
    assert_eq!(out.listing[2].addr, 3);
    assert_eq!(out.source_line_at(0), Some((0, 1)));
    assert_eq!(out.source_line_at(1), Some((0, 2)));
    assert_eq!(out.source_line_at(2), Some((0, 2)));
    assert_eq!(out.source_line_at(3), Some((0, 3)));
    assert_eq!(out.source_line_at(4), Some((0, 3))); // past the end: last line
}

#[test]
fn every_instruction_and_alias_form() {
    let src = r#"
        nop
        hlt
        rlc
        rrc
        ral
        rar
        daa
        cma
        stc
        cmc
        xchg
        xthl
        pchl
        sphl
        ret
        ei
        di
        rnz
        rz
        rnc
        rc
        rpo
        rpe
        rp
        rm
        rst 7
        in 8CH
        out 8DH
        mov a, m
        mov m, a
        mvi b, 12H
        lxi b, 1234H
        lxi sp, 0FFFFH
        stax b
        stax d
        ldax b
        ldax d
        dad b
        dad h
        dad sp
        inx b
        inx d
        inx h
        inx sp
        dcx b
        inr a
        inr m
        dcr b
        dcr m
        push b
        push d
        push h
        push psw
        pop b
        pop psw
        add a
        adc b
        sub c
        sbb d
        ana e
        xra h
        ora l
        cmp m
        adi 1
        aci 2
        sui 3
        sbi 4
        ani 5
        xri 6
        ori 7
        cpi 8
        jmp 0000H
        call 0000H
        jnz 1
        jz 2
        jnc 3
        jc 4
        jpo 5
        jpe 6
        jp 7
        jm 8
        cnz 1
        cz 2
        cnc 3
        cc 4
        cpo 5
        cpe 6
        cp 7
        cm 8
        shld 1
        lhld 2
        sta 3
        lda 4
"#;
    let (_, image) = asm(src).flat_image(0).unwrap();
    let expected: Vec<u8> = vec![
        0x00, 0x76, 0x07, 0x0F, 0x17, 0x1F, 0x27, 0x2F, 0x37, 0x3F, 0xEB, 0xE3, 0xE9, 0xF9, 0xC9,
        0xFB, 0xF3, 0xC0, 0xC8, 0xD0, 0xD8, 0xE0, 0xE8, 0xF0, 0xF8, 0xFF, 0xDB, 0x8C, 0xD3, 0x8D,
        0x7E, 0x77, 0x06, 0x12, 0x01, 0x34, 0x12, 0x31, 0xFF, 0xFF, 0x02, 0x12, 0x0A, 0x1A, 0x09,
        0x29, 0x39, 0x03, 0x13, 0x23, 0x33, 0x0B, 0x3C, 0x34, 0x05, 0x35, 0xC5, 0xD5, 0xE5, 0xF5,
        0xC1, 0xF1, 0x87, 0x88, 0x91, 0x9A, 0xA3, 0xAC, 0xB5, 0xBE, 0xC6, 0x01, 0xCE, 0x02, 0xD6,
        0x03, 0xDE, 0x04, 0xE6, 0x05, 0xEE, 0x06, 0xF6, 0x07, 0xFE, 0x08, 0xC3, 0x00, 0x00, 0xCD,
        0x00, 0x00, 0xC2, 0x01, 0x00, 0xCA, 0x02, 0x00, 0xD2, 0x03, 0x00, 0xDA, 0x04, 0x00, 0xE2,
        0x05, 0x00, 0xEA, 0x06, 0x00, 0xF2, 0x07, 0x00, 0xFA, 0x08, 0x00, 0xC4, 0x01, 0x00, 0xCC,
        0x02, 0x00, 0xD4, 0x03, 0x00, 0xDC, 0x04, 0x00, 0xE4, 0x05, 0x00, 0xEC, 0x06, 0x00, 0xF4,
        0x07, 0x00, 0xFC, 0x08, 0x00, 0x22, 0x01, 0x00, 0x2A, 0x02, 0x00, 0x32, 0x03, 0x00, 0x3A,
        0x04, 0x00,
    ];
    assert_eq!(image, expected);
}

#[test]
fn diagnostics_have_spans_and_dont_stop_the_world() {
    let mut mem = Mem::new("MVI A,MISSING\nDUP: NOP\nDUP: NOP\nMVI A,300");
    let out = assemble("main.asm", &mut mem);
    assert_eq!(out.diagnostics.len(), 3);
    assert!(out
        .diagnostics
        .iter()
        .any(|d| d.line == 1 && d.message.contains("MISSING")));
    assert!(out
        .diagnostics
        .iter()
        .any(|d| d.message.contains("duplicate")));
    assert!(out.diagnostics.iter().any(|d| d.message.contains("range")));
    // The image still has placeholder bytes of the right sizes.
    let (_, image) = out.flat_image(0).unwrap();
    assert_eq!(image, vec![0, 0, 0, 0, 0, 0]);
}

#[test]
fn overlapping_orgs_report() {
    let mut mem = Mem::new("ORG 10H\nDB 1,2,3,4\nORG 12H\nDB 5");
    let out = assemble("main.asm", &mut mem);
    assert!(out.diagnostics[0].message.contains("overlap"));
}

#[test]
fn number_forms() {
    let bytes = image("DB 10, 0x0A, 0AH, 1010B, 0b1010, 'A'");
    assert_eq!(bytes, vec![10, 10, 10, 10, 10, b'A']);
}

#[test]
fn lexer_drops_blank_and_comment_lines() {
    let out = asm("\n; just a comment\n  \n// another\nNOP\n");
    assert_eq!(out.listing.len(), 1);
    assert_eq!(out.listing[0].text, "NOP");
}

#[test]
fn tokens_carry_lengths_for_the_highlighter() {
    let mut diags = Vec::new();
    let lines = pmd85_asm::lex_lines(0, "LABEL: MVI A,1FH ; note\n", &mut diags);
    let toks = &lines[0].tokens;
    let lens: Vec<usize> = toks.iter().map(|t| t.len).collect();
    assert_eq!(lens, vec![5, 1, 3, 1, 1, 3]);
    // "LABEL: MVI A,1FH ; note": label, colon, opcode, operand,
    // comma, number — all with 1-based columns.
    let cols: Vec<usize> = toks.iter().map(|t| t.span.col).collect();
    assert_eq!(cols, vec![1, 6, 8, 12, 13, 14]);
}

#[test]
fn macros_substitute_parameters() {
    let bytes = image(
        "WAIT MACRO N\n\
         MVI A,N\n\
         DCR A\n\
         JNZ $-1\n\
         ENDM\n\
         ORG 10H\n\
         WAIT 5\n",
    );
    // At 0x10: MVI A,5 (2), DCR A (1), JNZ back to the DCR at 0x12.
    assert_eq!(bytes, vec![0x3E, 5, 0x3D, 0xC2, 0x12, 0x00]);
}

#[test]
fn macros_take_several_arguments() {
    let bytes = image(
        "PAIR MACRO A,B\n\
         DB A, B\n\
         ENDM\n\
         PAIR 10H, 20H\n",
    );
    assert_eq!(bytes, vec![0x10, 0x20]);
}

#[test]
fn macro_arguments_can_be_expressions_and_strings() {
    // Parenthesized commas stay inside one argument.
    let bytes = image(
        "M MACRO X\n\
         DB X\n\
         ENDM\n\
         M 1+(2*3)\n",
    );
    assert_eq!(bytes, vec![7]);
}

#[test]
fn macros_rename_local_labels_per_invocation() {
    // Two invocations of a body with a LOCAL label: each expands to
    // its own loop, jumping to its own label.
    let src = "SPIN MACRO N\n\
               LOCAL LOOP\n\
               MVI B,N\n\
               LOOP: DCR B\n\
               JNZ LOOP\n\
               ENDM\n\
               SPIN 3\n\
               SPIN 4\n";
    let out = asm(src);
    let bytes = out.flat_image(0).unwrap().1;
    assert_eq!(bytes[0], 0x06); // MVI B,3
    assert_eq!(bytes[1], 3);
    assert_eq!(bytes[2], 0x05); // DCR B at 2
    let jnz1 = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    let jnz2 = u16::from_le_bytes([bytes[10], bytes[11]]) as usize;
    assert_eq!(jnz1, 2, "first loop jumps to its own label");
    assert_eq!(jnz2, 8, "second loop jumps to its own label");
    assert_ne!(jnz1, jnz2);
}

#[test]
fn macros_can_invoke_macros() {
    let bytes = image(
        "INNER MACRO X\n\
         DB X\n\
         ENDM\n\
         OUTER MACRO X\n\
         INNER X+1\n\
         ENDM\n\
         OUTER 5\n",
    );
    assert_eq!(bytes, vec![6]);
}

#[test]
fn macro_invocations_can_carry_labels() {
    let bytes = image(
        "M MACRO X\n\
         DB X\n\
         ENDM\n\
         ORG 20H\n\
         HERE M 1FH\n",
    );
    assert_eq!(bytes, vec![0x1F]);
}

#[test]
fn macro_diagnostics_are_reported() {
    let d = |src: &str| {
        let mut mem = Mem::new(src);
        let out = assemble("main.asm", &mut mem);
        assert!(!out.diagnostics.is_empty(), "expected a diagnostic");
        out.diagnostics[0].message.clone()
    };
    assert!(d("ENDM\n").contains("without a MACRO"));
    assert!(d("LOCAL A\n").contains("inside a macro"));
    assert!(d("M MACRO\nDB 1\n").contains("no matching ENDM"));
    assert!(d("M MACRO A\nDB A\nENDM\nM 1, 2\n").contains("takes 1 argument"));
    assert!(d("M MACRO A\nM A\nENDM\nM 1\n").contains("nested deeper"));
    assert!(d("M MACRO\nA: MACRO\nENDM\nENDM\n").contains("nested MACRO"));
}

#[test]
fn comment_only_lines_lex_with_span_but_assemble_to_nothing() {
    // The highlighter needs comment-only lines in the stream (with
    // their span); the assembler must ignore them.
    let mut diags = Vec::new();
    let lines = pmd85_asm::lex_lines(0, "  ; hello\nNOP\n// bye", &mut diags);
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].comment, Some(3));
    assert!(lines[0].tokens.is_empty());
    assert_eq!(lines[2].comment, Some(1));
    let bytes = image("  ; hello\nNOP\n// bye\n");
    assert_eq!(bytes, vec![0x00]);
}
