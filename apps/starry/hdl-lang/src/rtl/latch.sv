// =============================================================================
// latch.sv -- level-sensitive transparent latch + a gated data capture.
//   Demonstrates: always_latch (IEEE 1800-2017 Cl.9.2.2.3), level-sensitive
//   behaviour (transparent when enable high, holds when low), parameter width.
//   Kept deliberately simple so Verilator and Icarus produce identical results.
// =============================================================================
module latch #(
  parameter int unsigned W = 8
) (
  input  logic         en,    // active-high transparent enable
  input  logic [W-1:0] d,
  output logic [W-1:0] q
);

  // always_latch: synthesizes a transparent latch. When `en` is high q follows
  // d; when low q retains its previous value (the missing else is intentional
  // and is precisely what makes it a latch).
  always_latch
    if (en) q = d;

endmodule : latch
