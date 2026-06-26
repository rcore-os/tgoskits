// =============================================================================
// hdl_pkg.sv -- shared package: data types, enums, structs, unions, params,
//               functions, tasks.  Doc-grounded in IEEE 1800-2017 SystemVerilog.
//
//   * package / endpackage  (Cl.26)
//   * typedef enum          (Cl.6.19)
//   * packed struct/union   (Cl.7.2 / 7.3)
//   * parameter / localparam (Cl.6.20)
//   * automatic function/task with input/output (Cl.13)
//   * integral data types: logic, bit, int, byte, shortint, longint, integer
//
// Portable across Verilator 5.008 and Icarus Verilog 12 (-g2012).
// All functions are pure / deterministic.
// =============================================================================
package hdl_pkg;

  // ---- localparam / parameter constants ------------------------------------
  localparam int unsigned DATA_W = 8;
  localparam int unsigned ADDR_W = 4;
  localparam int unsigned REGS   = 1 << ADDR_W;   // 16 registers

  // ---- enumerated ALU opcodes (typed enum, explicit encodings) -------------
  typedef enum logic [3:0] {
    OP_ADD  = 4'h0,
    OP_SUB  = 4'h1,
    OP_AND  = 4'h2,
    OP_OR   = 4'h3,
    OP_XOR  = 4'h4,
    OP_SHL  = 4'h5,
    OP_SHR  = 4'h6,
    OP_SAR  = 4'h7,   // arithmetic shift right
    OP_SLT  = 4'h8,   // set-less-than (signed)
    OP_MUL  = 4'h9,
    OP_NOT  = 4'hA,
    OP_PASS = 4'hF
  } alu_op_e;

  // ---- traffic-light FSM states (enum used by FSM module) -------------------
  typedef enum logic [1:0] {
    S_RED    = 2'd0,
    S_GREEN  = 2'd1,
    S_YELLOW = 2'd2
  } light_e;

  // ---- packed struct: an ALU status word -----------------------------------
  typedef struct packed {
    logic zero;
    logic carry;
    logic negative;
    logic overflow;
  } flags_t;

  // ---- packed struct: a tagged byte (nibble view) --------------------------
  typedef struct packed {
    logic [3:0] hi;
    logic [3:0] lo;
  } nibbles_t;

  // ---- packed union: a byte viewed as raw / as nibbles ---------------------
  typedef union packed {
    logic     [7:0] raw;
    nibbles_t       nib;
  } byteview_t;

  // ---- pure combinational ALU implemented as a function --------------------
  // Mirrors the alu module; used by the TB as an independent reference model.
  function automatic logic [DATA_W-1:0] alu_compute(
      input alu_op_e            op,
      input logic [DATA_W-1:0]  x,
      input logic [DATA_W-1:0]  y);
    logic signed [DATA_W-1:0] sx;
    logic signed [DATA_W-1:0] sy;
    sx = x;
    sy = y;
    unique case (op)
      OP_ADD : return x + y;
      OP_SUB : return x - y;
      OP_AND : return x & y;
      OP_OR  : return x | y;
      OP_XOR : return x ^ y;
      OP_SHL : return x << y[2:0];
      OP_SHR : return x >> y[2:0];
      OP_SAR : return sx >>> y[2:0];
      OP_SLT : return (sx < sy) ? {{(DATA_W-1){1'b0}}, 1'b1} : '0;
      OP_MUL : return x * y;
      OP_NOT : return ~x;
      OP_PASS: return x;
      default: return '0;
    endcase
  endfunction

  // ---- recursive-free factorial via while (loop construct demo) ------------
  function automatic int unsigned factorial(input int unsigned n);
    int unsigned r;
    int unsigned k;
    r = 1;
    k = n;
    while (k > 1) begin
      r = r * k;
      k--;
    end
    return r;
  endfunction

  // ---- population count via for-loop ---------------------------------------
  function automatic int unsigned popcount(input logic [DATA_W-1:0] v);
    int unsigned c;
    c = 0;
    for (int i = 0; i < DATA_W; i++) begin
      if (v[i]) c++;
    end
    return c;
  endfunction

  // ---- gcd via Euclid's algorithm (bounded while-loop; portable to Icarus
  //      12 which lacks break/continue) --------------------------------------
  function automatic int unsigned gcd(input int unsigned a, input int unsigned b);
    int unsigned x;
    int unsigned y;
    int unsigned t;
    x = a;
    y = b;
    while (y != 0) begin
      t = x % y;
      x = y;
      y = t;
    end
    return x;
  endfunction

  // ---- task with output args (compute sum and max) -------------------------
  task automatic sum_and_max(
      input  logic [DATA_W-1:0] a,
      input  logic [DATA_W-1:0] b,
      output logic [DATA_W:0]   s,
      output logic [DATA_W-1:0] m);
    s = a + b;
    m = (a > b) ? a : b;
  endtask

endpackage : hdl_pkg
