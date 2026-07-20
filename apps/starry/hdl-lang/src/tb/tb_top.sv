// =============================================================================
// tb_top.sv -- the carpet testbench. Drives every design module and exercises
// SystemVerilog language constructs directly, printing DETERMINISTIC results on
// lines prefixed "TB:" so host golden-capture compares only these lines (and
// not the simulator-specific $finish epilogue).
//
// Sections:
//   A. literals / number formats / operators
//   B. data types (logic/bit/int/byte/shortint/longint/integer/real), casts
//   C. packed arrays / unpacked arrays / foreach
//   D. structs / unions / enums
//   E. control flow: if/else, case/casez/casex, unique/priority, for/while/
//      repeat/do-while/foreach, break/continue
//   F. functions / tasks from package (recursion-free loops, output args)
//   G. ALU module (every opcode) + flags + immediate assertions
//   H. register file module (write/read/reset)
//   I. counter module (up/down/load/hold/terminal-count)
//   J. FSM #1 traffic light (Moore)
//   K. FSM #2 sequence detector "1011" (Mealy)
//   L. barrel shifter module (shl/shr/sar/rotate, multiple widths)
//   M. generate-block ripple adder + parity (generate-for / generate-if)
//   N. system functions ($countones/$onehot/$clog2/$sformatf/$bits ...)
//
// Determinism: no $time printed, no $random, no addresses, fixed stimuli only.
// =============================================================================
`timescale 1ns/1ps

module tb_top
  import hdl_pkg::*;
;

  // ---- a free-running clock for the sequential DUTs -------------------------
  logic clk = 1'b0;
  always #5 clk = ~clk;

  integer pass_count = 0;
  integer fail_count = 0;

  // helper: deterministic self-check that prints a TB: line + tallies
  task automatic expect_eq(input string name,
                           input longint unsigned got,
                           input longint unsigned exp);
    if (got === exp) begin
      pass_count = pass_count + 1;
      $display("TB: %s = %0d OK", name, got);
    end else begin
      fail_count = fail_count + 1;
      $display("TB: %s = %0d FAIL(exp %0d)", name, got, exp);
    end
  endtask

  // ===========================================================================
  // DUT instances
  // ===========================================================================
  // -- ALU --
  alu_op_e         alu_op;
  logic [7:0]      alu_a, alu_b, alu_y;
  flags_t          alu_f;
  alu #(.W(8)) u_alu (.op(alu_op), .a(alu_a), .b(alu_b), .y(alu_y), .flags(alu_f));

  // -- register file --
  logic        rf_rst, rf_we;
  logic [3:0]  rf_wa, rf_ra0, rf_ra1;
  logic [7:0]  rf_wd, rf_rd0, rf_rd1;
  regfile #(.W(8), .N(16), .AW(4)) u_rf (
    .clk(clk), .rst(rf_rst), .we(rf_we),
    .waddr(rf_wa), .wdata(rf_wd),
    .raddr0(rf_ra0), .raddr1(rf_ra1),
    .rdata0(rf_rd0), .rdata1(rf_rd1));

  // -- counter --
  logic        c_rst, c_load, c_en, c_up, c_tc;
  logic [7:0]  c_din, c_q;
  counter #(.W(8)) u_cnt (
    .clk(clk), .rst(c_rst), .load(c_load), .en(c_en), .up(c_up),
    .din(c_din), .q(c_q), .tc(c_tc));

  // -- traffic FSM --
  logic   tf_rst, tf_red, tf_grn, tf_yel;
  light_e tf_state;
  fsm_traffic #(.T_RED(3), .T_GREEN(3), .T_YELLOW(2)) u_tf (
    .clk(clk), .rst(tf_rst), .state(tf_state),
    .red(tf_red), .green(tf_grn), .yellow(tf_yel));

  // -- sequence detector FSM --
  logic sd_rst, sd_din, sd_found;
  fsm_seqdet u_sd (.clk(clk), .rst(sd_rst), .din(sd_din), .found(sd_found));

  // -- barrel shifters (two widths) --
  logic [7:0]  sh8_data, sh8_res;
  logic [2:0]  sh8_amt;
  logic        sh8_left, sh8_arith, sh8_rot;
  shifter #(.W(8)) u_sh8 (
    .data(sh8_data), .amt(sh8_amt), .left(sh8_left),
    .arith(sh8_arith), .rotate(sh8_rot), .result(sh8_res));

  logic [15:0] sh16_data, sh16_res;
  logic [3:0]  sh16_amt;
  logic        sh16_left, sh16_arith, sh16_rot;
  shifter #(.W(16)) u_sh16 (
    .data(sh16_data), .amt(sh16_amt), .left(sh16_left),
    .arith(sh16_arith), .rotate(sh16_rot), .result(sh16_res));

  // -- generate-block ripple adder --
  logic [7:0] ga_a, ga_b, ga_sum;
  logic       ga_cin, ga_cout, ga_par;
  genblk #(.W(8), .WITH_PAR(1'b1)) u_gb (
    .a(ga_a), .b(ga_b), .cin(ga_cin),
    .sum(ga_sum), .cout(ga_cout), .parity(ga_par));

  // -- transparent latch (always_latch) --
  logic       lt_en;
  logic [7:0] lt_d, lt_q;
  latch #(.W(8)) u_lt (.en(lt_en), .d(lt_d), .q(lt_q));

  // ===========================================================================
  // local scratch for the language-feature sections
  // ===========================================================================
  bit            bv;
  byte           sbyte;
  shortint       sh_i;
  int            i32;
  longint        l64;
  integer        ivar;
  real           rval;
  logic [7:0]    packed_a, packed_b;
  logic [7:0]    uarr [0:3];
  logic [3:0]    marr [0:1][0:1];
  nibbles_t      nib;
  byteview_t     bview;
  alu_op_e       eop;
  string         str;
  int unsigned   acc;
  int            loopcnt;

  initial begin
    // =========================================================================
    // SECTION A: literals, number formats, operators
    // =========================================================================
    expect_eq("A.hex_lit",   8'hA5,              165);
    expect_eq("A.bin_lit",   8'b1010_0101,       165);
    expect_eq("A.oct_lit",   8'o245,             165);
    expect_eq("A.dec_lit",   8'd165,             165);
    expect_eq("A.sized_24",  24'h00_AB_CD,       16'hABCD);
    expect_eq("A.add",       8'd9 + 8'd5,         14);
    expect_eq("A.sub",       8'd9 - 8'd5,          4);
    expect_eq("A.mul",       8'd9 * 8'd5,         45);
    expect_eq("A.div",       8'd45 / 8'd5,         9);
    expect_eq("A.mod",       8'd45 % 8'd7,         3);
    expect_eq("A.pow",       3 ** 4,              81);
    expect_eq("A.and",       8'hF0 & 8'h3C,    8'h30);
    expect_eq("A.or",        8'hF0 | 8'h0C,    8'hFC);
    expect_eq("A.xor",       8'hFF ^ 8'h0F,    8'hF0);
    expect_eq("A.shl",       8'h01 << 4,       8'h10);
    expect_eq("A.shr",       8'h80 >> 3,       8'h10);
    expect_eq("A.reduce_and",(& 8'hFF),            1);
    expect_eq("A.reduce_or", (| 8'h00),            0);
    // 1011_0010 has 4 set bits -> XOR-reduction = 0 (verified on both sims)
    expect_eq("A.reduce_xor",(^ 8'b1011_0010),     0);
    expect_eq("A.eq",        (8'd5 == 8'd5),       1);
    expect_eq("A.neq",       (8'd5 != 8'd6),       1);
    expect_eq("A.lt",        (8'd5 <  8'd6),       1);
    expect_eq("A.ternary",   (1'b1 ? 8'd7 : 8'd9), 7);
    expect_eq("A.concat",    {4'hA, 4'h5},      8'hA5);
    expect_eq("A.replicate", {4{2'b10}},        8'b10101010);
    expect_eq("A.logand",    (1'b1 && 1'b1),       1);
    expect_eq("A.logor",     (1'b0 || 1'b1),       1);
    expect_eq("A.lognot",    (!1'b0),              1);

    // =========================================================================
    // SECTION B: data types + casts (sizes & signedness)
    // =========================================================================
    bv     = 1'b1;            expect_eq("B.bit",      bv,               1);
    sbyte  = -8'sd5;          expect_eq("B.byte_neg", sbyte & 8'hFF,  251); // -5 in 2s comp
    sh_i   = 16'sd1000;       expect_eq("B.shortint", sh_i,          1000);
    i32    = 32'd1_000_000;   expect_eq("B.int",      i32,        1000000);
    l64    = 64'd9_000_000_000; expect_eq("B.longint", l64, 64'd9_000_000_000);
    ivar   = 12345;           expect_eq("B.integer",  ivar,         12345);
    expect_eq("B.bits_int",   $bits(int),     32);
    expect_eq("B.bits_byte",  $bits(byte),     8);
    expect_eq("B.bits_long",  $bits(longint), 64);
    rval   = 2.5 * 4.0;       expect_eq("B.real_int", int'(rval),     10);
    expect_eq("B.real_round_up",  int'(7.6),    8);
    expect_eq("B.real_round_dn",  int'(7.4),    7);
    expect_eq("B.signed_cast",    $unsigned(-8'sd1), 255);
    expect_eq("B.signed_div",     $unsigned(-17 / 4) & 32'hFF, 252); // -4
    expect_eq("B.ashr_signed",    $unsigned(-8'sd16 >>> 2) & 8'hFF, 252); // -4

    // =========================================================================
    // SECTION C: packed & unpacked arrays, foreach, slices
    // =========================================================================
    packed_a = 8'hC3;
    expect_eq("C.bit_select",   packed_a[7],          1);
    expect_eq("C.part_select",  packed_a[3:0],     8'h3);
    expect_eq("C.indexed_pos",  packed_a[4 +: 4],  8'hC);
    expect_eq("C.indexed_neg",  packed_a[7 -: 4],  8'hC);
    uarr[0]=8'd1; uarr[1]=8'd2; uarr[2]=8'd4; uarr[3]=8'd8;
    acc = 0;
    foreach (uarr[idx]) acc += uarr[idx];
    expect_eq("C.foreach_sum",  acc,                 15);
    marr[0][0]=4'd1; marr[0][1]=4'd2; marr[1][0]=4'd3; marr[1][1]=4'd4;
    acc = 0;
    foreach (marr[r, c]) acc += marr[r][c];
    expect_eq("C.foreach_2d",   acc,                 10);

    // =========================================================================
    // SECTION D: structs, unions, enums
    // =========================================================================
    nib.hi = 4'hD; nib.lo = 4'h6;
    expect_eq("D.struct_hi",    nib.hi,            8'hD);
    expect_eq("D.struct_lo",    nib.lo,            8'h6);
    expect_eq("D.struct_whole", nib,              8'hD6);
    bview.raw = 8'hF0;
    expect_eq("D.union_raw",    bview.raw,         8'hF0);
    expect_eq("D.union_nibhi",  bview.nib.hi,      8'hF);
    expect_eq("D.union_niblo",  bview.nib.lo,      8'h0);
    eop = OP_XOR;
    expect_eq("D.enum_val",     eop,               8'h4);
    expect_eq("D.enum_pass",    OP_PASS,           8'hF);

    // =========================================================================
    // SECTION E: control flow
    // =========================================================================
    // if / else-if chain
    i32 = 50;
    if      (i32 < 10) ivar = 0;
    else if (i32 < 100) ivar = 1;
    else               ivar = 2;
    expect_eq("E.if_elif", ivar, 1);

    // case
    packed_a = 8'h02;
    case (packed_a)
      8'h01: ivar = 10;
      8'h02: ivar = 20;
      8'h03: ivar = 30;
      default: ivar = 99;
    endcase
    expect_eq("E.case", ivar, 20);

    // casez (wildcards in items)
    packed_a = 8'b1010_0000;
    casez (packed_a)
      8'b1???_????: ivar = 1;
      8'b01??_????: ivar = 2;
      default:      ivar = 0;
    endcase
    expect_eq("E.casez", ivar, 1);

    // casex: 'x' in an item matches any value. With value 0011_0000:
    //   item xx10_????  needs bits[5:4]==10, but they are 11 -> no match
    //   item 0011_????  matches -> ivar = 5 (verified on both sims)
    packed_a = 8'b0011_0000;
    casex (packed_a)
      8'bxx10_????: ivar = 7;   // x matches anything in casex
      8'b0011_????: ivar = 5;
      default:      ivar = 0;
    endcase
    expect_eq("E.casex", ivar, 5);

    // unique case
    packed_a = 8'h03;
    unique case (packed_a)
      8'h01: ivar = 1;
      8'h03: ivar = 3;
      default: ivar = 0;
    endcase
    expect_eq("E.unique", ivar, 3);

    // priority case
    packed_a = 8'b0000_0110;
    priority case (1'b1)
      packed_a[0]: ivar = 0;
      packed_a[1]: ivar = 1;   // first set bit from top of list
      packed_a[2]: ivar = 2;
      default:     ivar = 9;
    endcase
    expect_eq("E.priority", ivar, 1);

    // for loop (explicit label -> stable, unique scope name in both sims)
    acc = 0;
    begin : blk_for
      for (int kf = 1; kf <= 10; kf++) acc += kf;
    end
    expect_eq("E.for", acc, 55);

    // while loop
    acc = 0; loopcnt = 16;
    while (loopcnt > 0) begin acc += 1; loopcnt = loopcnt >> 1; end
    expect_eq("E.while", acc, 5);

    // repeat loop
    acc = 0;
    repeat (7) acc += 3;
    expect_eq("E.repeat", acc, 21);

    // do-while
    acc = 0; loopcnt = 0;
    do begin acc += 2; loopcnt++; end while (loopcnt < 4);
    expect_eq("E.dowhile", acc, 8);

    // bounded loop with guard flags (portable: Icarus 12 has no break/continue,
    // so emulate "break at k==5, continue on even k" with explicit conditions).
    // sums odd k < 5 -> 1 + 3 = 4
    acc = 0;
    begin : blk_loop_guarded
      bit stop;
      stop = 1'b0;
      for (int kg = 0; kg < 20; kg++) begin
        if (kg == 5)            stop = 1'b1;   // emulate break
        if (!stop && (kg % 2))  acc += kg;     // skip even (emulate continue)
      end
    end
    expect_eq("E.loop_guarded", acc, 4);

    // =========================================================================
    // SECTION F: package functions / tasks
    // =========================================================================
    expect_eq("F.factorial5", factorial(5),             120);
    expect_eq("F.factorial0", factorial(0),               1);
    expect_eq("F.popcount",   popcount(8'b1011_0010),     4);
    expect_eq("F.gcd",        gcd(48, 36),               12);
    expect_eq("F.alu_fn_add", alu_compute(OP_ADD, 8'd20, 8'd22), 42);
    expect_eq("F.alu_fn_mul", alu_compute(OP_MUL, 8'd6,  8'd7),  42);
    begin : blk_task
      logic [8:0] tsum; logic [7:0] tmax;
      sum_and_max(8'd200, 8'd100, tsum, tmax);
      expect_eq("F.task_sum", tsum, 300);
      expect_eq("F.task_max", tmax, 200);
    end

    // =========================================================================
    // SECTION G: ALU module -- exercise EVERY opcode + flags
    // =========================================================================
    alu_op = OP_ADD; alu_a = 8'd100; alu_b = 8'd28;  #1;
    expect_eq("G.add",       alu_y,             128);
    expect_eq("G.add_neg",   alu_f.negative,      1); // bit7 set
    alu_op = OP_ADD; alu_a = 8'hFF; alu_b = 8'h02;  #1;
    expect_eq("G.add_carry", alu_f.carry,         1);
    alu_op = OP_SUB; alu_a = 8'd50; alu_b = 8'd50;  #1;
    expect_eq("G.sub_zero",  alu_y,               0);
    expect_eq("G.sub_zflag", alu_f.zero,          1);
    alu_op = OP_AND; alu_a = 8'hF0; alu_b = 8'h3C;  #1;
    expect_eq("G.and",       alu_y,           8'h30);
    alu_op = OP_OR;  alu_a = 8'hF0; alu_b = 8'h0C;  #1;
    expect_eq("G.or",        alu_y,           8'hFC);
    alu_op = OP_XOR; alu_a = 8'hFF; alu_b = 8'h0F;  #1;
    expect_eq("G.xor",       alu_y,           8'hF0);
    alu_op = OP_SHL; alu_a = 8'h01; alu_b = 8'd4;   #1;
    expect_eq("G.shl",       alu_y,           8'h10);
    alu_op = OP_SHR; alu_a = 8'h80; alu_b = 8'd3;   #1;
    expect_eq("G.shr",       alu_y,           8'h10);
    alu_op = OP_SAR; alu_a = 8'hF0; alu_b = 8'd2;   #1; // -16 >>> 2 = -4 = 0xFC
    expect_eq("G.sar",       alu_y,           8'hFC);
    alu_op = OP_SLT; alu_a = 8'hFF; alu_b = 8'h01;  #1; // -1 < 1 -> 1
    expect_eq("G.slt_true",  alu_y,               1);
    alu_op = OP_SLT; alu_a = 8'h05; alu_b = 8'h02;  #1; // 5 < 2 -> 0
    expect_eq("G.slt_false", alu_y,               0);
    alu_op = OP_MUL; alu_a = 8'd6;  alu_b = 8'd7;   #1;
    expect_eq("G.mul",       alu_y,              42);
    alu_op = OP_NOT; alu_a = 8'h0F; alu_b = 8'h00;  #1;
    expect_eq("G.not",       alu_y,           8'hF0);
    alu_op = OP_PASS; alu_a = 8'hAB; alu_b = 8'h00; #1;
    expect_eq("G.pass",      alu_y,           8'hAB);
    // immediate assertion: PASS is identity
    assert (alu_y == 8'hAB)
      else $display("TB: G.assert_pass FAIL");
    // overflow flag: 100 + 50 (both positive) -> 150 = 0x96, bit7 set => signed ovf
    alu_op = OP_ADD; alu_a = 8'd100; alu_b = 8'd50; #1;
    expect_eq("G.add_ovf",   alu_f.overflow,      1);

    // =========================================================================
    // SECTION H: register file (synchronous write, async read, reset)
    // =========================================================================
    rf_rst = 1; rf_we = 0; rf_wa = 0; rf_wd = 0; rf_ra0 = 0; rf_ra1 = 0;
    @(posedge clk); #1;           // reset clears all
    rf_rst = 0;
    // write reg[3] = 0xAA
    rf_we = 1; rf_wa = 4'd3; rf_wd = 8'hAA; @(posedge clk); #1;
    // write reg[7] = 0x55
    rf_wa = 4'd7; rf_wd = 8'h55; @(posedge clk); #1;
    rf_we = 0;
    rf_ra0 = 4'd3; rf_ra1 = 4'd7; #1;
    expect_eq("H.rd_port0",  rf_rd0, 8'hAA);
    expect_eq("H.rd_port1",  rf_rd1, 8'h55);
    rf_ra0 = 4'd0; #1;
    expect_eq("H.rd_zero",   rf_rd0, 8'h00); // untouched after reset
    // overwrite reg[3]
    rf_we = 1; rf_wa = 4'd3; rf_wd = 8'h11; @(posedge clk); #1;
    rf_we = 0; rf_ra0 = 4'd3; #1;
    expect_eq("H.rd_overwrite", rf_rd0, 8'h11);

    // =========================================================================
    // SECTION I: counter (reset/load/up/down/hold/terminal-count)
    // =========================================================================
    c_rst = 1; c_load = 0; c_en = 0; c_up = 1; c_din = 0;
    @(posedge clk); #1;
    expect_eq("I.reset", c_q, 0);
    c_rst = 0;
    // count up 5 times
    c_en = 1;
    repeat (5) @(posedge clk);
    #1; expect_eq("I.up5", c_q, 5);
    // hold
    c_en = 0; @(posedge clk); #1;
    expect_eq("I.hold", c_q, 5);
    // load 250
    c_load = 1; c_din = 8'd250; @(posedge clk); #1;
    c_load = 0;
    expect_eq("I.load", c_q, 250);
    // count up to 255 then terminal count
    c_en = 1; c_up = 1;
    repeat (5) @(posedge clk);
    #1; expect_eq("I.tc_val", c_q, 255);
    expect_eq("I.tc_flag", c_tc, 1);
    // count down 3
    c_up = 0; repeat (3) @(posedge clk); #1;
    expect_eq("I.down3", c_q, 252);
    c_en = 0;

    // =========================================================================
    // SECTION J: traffic-light FSM (Moore, timed)
    // =========================================================================
    tf_rst = 1; @(posedge clk); #1;
    expect_eq("J.reset_red", tf_red, 1);
    expect_eq("J.state_red", tf_state, S_RED);
    tf_rst = 0;
    // RED dwell = 3 cycles, then GREEN
    repeat (3) @(posedge clk); #1;
    expect_eq("J.green",     tf_grn, 1);
    expect_eq("J.state_grn", tf_state, S_GREEN);
    // GREEN dwell = 3 -> YELLOW
    repeat (3) @(posedge clk); #1;
    expect_eq("J.yellow",    tf_yel, 1);
    expect_eq("J.state_yel", tf_state, S_YELLOW);
    // YELLOW dwell = 2 -> RED
    repeat (2) @(posedge clk); #1;
    expect_eq("J.back_red",  tf_red, 1);
    expect_eq("J.state_red2",tf_state, S_RED);

    // =========================================================================
    // SECTION K: sequence detector FSM (Mealy, pattern 1011, overlapping)
    // =========================================================================
    sd_rst = 1; sd_din = 0; @(posedge clk); #1;
    sd_rst = 0;
    begin : blk_seqdet
      // stream: 1 0 1 1 0 1 1   -> detect "1011" at positions:
      //   bits:  1 0 1 1 -> found (after 4th)
      //   then overlap: ...1 0 1 1 again -> found
      bit [0:6] stream;
      int match_cnt;
      stream = 7'b1011011;
      match_cnt = 0;
      for (int s = 0; s < 7; s++) begin
        sd_din = stream[s];
        @(posedge clk); #1;
        if (sd_found) match_cnt = match_cnt + 1;
      end
      expect_eq("K.matches", match_cnt, 2);
    end

    // =========================================================================
    // SECTION L: barrel shifter (logical/arith/rotate, two widths)
    // =========================================================================
    // 8-bit logical left by 3: 0x01 << 3 = 0x08
    sh8_data = 8'h01; sh8_amt = 3'd3; sh8_left = 1; sh8_arith = 0; sh8_rot = 0; #1;
    expect_eq("L.shl8",  sh8_res, 8'h08);
    // 8-bit logical right by 4: 0x80 >> 4 = 0x08
    sh8_data = 8'h80; sh8_amt = 3'd4; sh8_left = 0; sh8_arith = 0; sh8_rot = 0; #1;
    expect_eq("L.shr8",  sh8_res, 8'h08);
    // 8-bit arithmetic right by 2: 0xF0 (-16) >>> 2 = 0xFC (-4)
    sh8_data = 8'hF0; sh8_amt = 3'd2; sh8_left = 0; sh8_arith = 1; sh8_rot = 0; #1;
    expect_eq("L.sar8",  sh8_res, 8'hFC);
    // 8-bit rotate left by 4: 0xC3 rol 4 = 0x3C
    sh8_data = 8'hC3; sh8_amt = 3'd4; sh8_left = 1; sh8_arith = 0; sh8_rot = 1; #1;
    expect_eq("L.rol8",  sh8_res, 8'h3C);
    // 8-bit rotate right by 1: 0x01 ror 1 = 0x80
    sh8_data = 8'h01; sh8_amt = 3'd1; sh8_left = 0; sh8_arith = 0; sh8_rot = 1; #1;
    expect_eq("L.ror8",  sh8_res, 8'h80);
    // 16-bit logical left by 8: 0x00FF << 8 = 0xFF00
    sh16_data = 16'h00FF; sh16_amt = 4'd8; sh16_left = 1; sh16_arith = 0; sh16_rot = 0; #1;
    expect_eq("L.shl16", sh16_res, 16'hFF00);
    // 16-bit rotate right by 4: 0x000F ror 4 = 0xF000
    sh16_data = 16'h000F; sh16_amt = 4'd4; sh16_left = 0; sh16_arith = 0; sh16_rot = 1; #1;
    expect_eq("L.ror16", sh16_res, 16'hF000);

    // =========================================================================
    // SECTION M: generate-block ripple adder + parity
    // =========================================================================
    ga_a = 8'd200; ga_b = 8'd100; ga_cin = 1'b0; #1;
    expect_eq("M.sum",    ga_sum,  8'd44);   // (300 mod 256)
    expect_eq("M.cout",   ga_cout,    1);    // carry out
    ga_a = 8'd15; ga_b = 8'd9; ga_cin = 1'b1; #1; // 15+9+1 = 25
    expect_eq("M.sum_cin",ga_sum,    25);
    expect_eq("M.parity", ga_par, (^8'd25)); // parity of 25 = 0b00011001 -> 3 ones -> 1
    ga_a = 8'h00; ga_b = 8'h00; ga_cin = 1'b0; #1;
    expect_eq("M.zero",   ga_sum,     0);
    expect_eq("M.par0",   ga_par,     0);

    // =========================================================================
    // SECTION O: transparent latch (always_latch) -- level-sensitive hold
    // =========================================================================
    // transparent: en high -> q follows d
    lt_en = 1'b1; lt_d = 8'hA5; #1;
    expect_eq("O.transparent", lt_q, 8'hA5);
    // hold: en low -> q retains last value even as d changes
    lt_en = 1'b0; lt_d = 8'h5A; #1;
    expect_eq("O.hold",        lt_q, 8'hA5);
    // re-open: en high again -> captures new d
    lt_en = 1'b1; lt_d = 8'h3C; #1;
    expect_eq("O.reopen",      lt_q, 8'h3C);

    // =========================================================================
    // SECTION N: system functions
    // =========================================================================
    expect_eq("N.countones", $countones(8'b1011_0010), 4);
    expect_eq("N.onehot1",   $onehot(8'b0001_0000),    1);
    expect_eq("N.onehot0a",  $onehot(8'b0011_0000),    0);
    expect_eq("N.onehot0z",  $onehot0(8'b0000_0000),   1);
    expect_eq("N.clog2_100", $clog2(100),              7);
    expect_eq("N.clog2_256", $clog2(256),              8);
    expect_eq("N.bits_flags",$bits(flags_t),           4);
    expect_eq("N.bits_byteview",$bits(byteview_t),     8);
    str = $sformatf("v=%0d w=%h", 42, 8'hAB);
    if (str == "v=42 w=ab") begin
      pass_count++; $display("TB: N.sformatf = OK");
    end else begin
      fail_count++; $display("TB: N.sformatf = FAIL(%s)", str);
    end
    // sign extension: assigning signed 8-bit -1 to a signed 32-bit var fills
    // the upper bits with 1 -> 0xFFFFFFFF == 4294967295 (verified on both sims)
    begin : blk_sign_extend
      logic signed [7:0]  se8;
      logic signed [31:0] se32;
      se8  = 8'hFF;
      se32 = se8;
      expect_eq("N.sign_extend", $unsigned(se32), 32'd4294967295);
    end

    // =========================================================================
    // FINAL TALLY
    // =========================================================================
    $display("TB: TOTAL pass=%0d fail=%0d", pass_count, fail_count);
    if (fail_count == 0)
      $display("TB: CARPET_RESULT ALL_PASS");
    else
      $display("TB: CARPET_RESULT HAS_FAIL");
    $finish;
  end

endmodule : tb_top
