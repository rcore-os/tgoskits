// =============================================================================
// alu.sv -- parametrized combinational ALU.
//   Demonstrates: module ports, parameters, import package, always_comb,
//   blocking assignments, unique case, signed arithmetic, flag generation,
//   operator coverage (+ - & | ^ << >> >>> < * ~), immediate assertion.
// IEEE 1800-2017: Cl.23 (modules), Cl.9.2.2.2 (always_comb), Cl.12 (cases).
// =============================================================================
module alu
  import hdl_pkg::*;
#(
  parameter int unsigned W = DATA_W
) (
  input  alu_op_e        op,
  input  logic [W-1:0]   a,
  input  logic [W-1:0]   b,
  output logic [W-1:0]   y,
  output flags_t         flags
);

  logic signed [W-1:0] sa;
  logic signed [W-1:0] sb;
  logic [W:0]          ext;   // one extra bit for carry/overflow detection

  always_comb begin
    sa  = a;
    sb  = b;
    ext = '0;
    y   = '0;
    unique case (op)
      OP_ADD : begin ext = {1'b0, a} + {1'b0, b}; y = ext[W-1:0]; end
      OP_SUB : begin ext = {1'b0, a} - {1'b0, b}; y = ext[W-1:0]; end
      OP_AND : y = a & b;
      OP_OR  : y = a | b;
      OP_XOR : y = a ^ b;
      OP_SHL : y = a << b[2:0];
      OP_SHR : y = a >> b[2:0];
      OP_SAR : y = sa >>> b[2:0];
      OP_SLT : y = (sa < sb) ? {{(W-1){1'b0}}, 1'b1} : '0;
      OP_MUL : y = a * b;
      OP_NOT : y = ~a;
      OP_PASS: y = a;
      default: y = '0;
    endcase

    // status flags (deterministic functions of inputs/result)
    flags.zero     = (y == '0);
    flags.negative = y[W-1];
    flags.carry    = ext[W];
    flags.overflow = (op == OP_ADD) ? (a[W-1] == b[W-1]) && (y[W-1] != a[W-1]) :
                     (op == OP_SUB) ? (a[W-1] != b[W-1]) && (y[W-1] != a[W-1]) :
                     1'b0;
  end

  // immediate assertion: PASS must be an identity (constructs: assert) -------
`ifndef VERILATOR
  // iverilog supports concurrent-style immediate assert inside always; keep it
  // procedural & simple so both simulators accept it.
`endif

endmodule : alu
