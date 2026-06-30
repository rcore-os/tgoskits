// =============================================================================
// genblk.sv -- generate constructs showcase.
//   Demonstrates: generate-for instantiating submodules (ripple of full
//   adders), generate-if (conditional generation), parameter-driven structure,
//   nested generate, named blocks, hierarchical references in TB via dotted
//   names are avoided for portability -- outputs are flattened to ports.
// IEEE 1800-2017: Cl.27.5 (loop generate), Cl.27.4 (conditional generate).
// =============================================================================

// single-bit full adder (leaf used by the ripple-carry generate-for)
module full_adder (
  input  logic a,
  input  logic b,
  input  logic cin,
  output logic sum,
  output logic cout
);
  assign sum  = a ^ b ^ cin;
  assign cout = (a & b) | (a & cin) | (b & cin);
endmodule : full_adder

module genblk #(
  parameter int unsigned W        = 8,
  parameter bit          WITH_PAR = 1'b1   // generate-if toggle
) (
  input  logic [W-1:0] a,
  input  logic [W-1:0] b,
  input  logic         cin,
  output logic [W-1:0] sum,
  output logic         cout,
  output logic         parity    // optional parity bit (generate-if)
);

  logic [W:0] carry;
  assign carry[0] = cin;

  // loop-generate: ripple-carry adder built from W full_adder instances
  genvar i;
  generate
    for (i = 0; i < int'(W); i++) begin : g_fa
      full_adder fa (
        .a   (a[i]),
        .b   (b[i]),
        .cin (carry[i]),
        .sum (sum[i]),
        .cout(carry[i+1])
      );
    end : g_fa
  endgenerate

  assign cout = carry[W];

  // conditional-generate: only build the parity tree when WITH_PAR
  generate
    if (WITH_PAR) begin : g_par
      assign parity = ^sum;          // XOR-reduction
    end else begin : g_nopar
      assign parity = 1'b0;
    end
  endgenerate

endmodule : genblk
