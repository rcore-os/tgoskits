// =============================================================================
// fsm_seqdet.sv -- Mealy "1011" overlapping sequence detector.
//   Demonstrates: a second, structurally different FSM (Mealy: output depends
//   on state AND input), enum states declared inline, casez-free clean case,
//   single-process style with combinational Mealy output, registered state.
// IEEE 1800-2017: Cl.6.19, Cl.9.2.2.4, Cl.12.5.
// =============================================================================
module fsm_seqdet (
  input  logic clk,
  input  logic rst,
  input  logic din,
  output logic found      // 1 in the cycle the pattern 1011 completes
);

  typedef enum logic [2:0] {
    D_IDLE = 3'd0,   // seen nothing
    D_1    = 3'd1,   // seen 1
    D_10   = 3'd2,   // seen 10
    D_101  = 3'd3    // seen 101
  } dstate_e;

  dstate_e cur, nxt;

  always_ff @(posedge clk or posedge rst)
    if (rst) cur <= D_IDLE;
    else     cur <= nxt;

  // Mealy next-state + output
  always_comb begin
    nxt   = cur;
    found = 1'b0;
    // if/else used (instead of ?:) so enum-typed assignments need no explicit
    // cast -- Icarus 12 rejects an enum LHS fed by a ?: of enum literals.
    unique case (cur)
      D_IDLE: if (din) nxt = D_1;   else nxt = D_IDLE;
      D_1:    if (din) nxt = D_1;   else nxt = D_10;
      D_10:   if (din) nxt = D_101; else nxt = D_IDLE;
      D_101:  begin
                if (din) begin nxt = D_1; found = 1'b1; end  // 1011 done, overlap -> "1"
                else           nxt = D_10;
              end
      default: nxt = D_IDLE;
    endcase
  end

endmodule : fsm_seqdet
