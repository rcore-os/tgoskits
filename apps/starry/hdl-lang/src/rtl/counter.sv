// =============================================================================
// counter.sv -- parametrized up/down counter with async reset, load, enable.
//   Demonstrates: parameters, async reset (posedge clk or posedge rst),
//   nonblocking assignment, priority if/else if, mux on direction,
//   width-parametrized arithmetic, terminal-count output.
// IEEE 1800-2017: Cl.9.2.2.4 (always_ff), Cl.12.4 (if).
// =============================================================================
module counter #(
  parameter int unsigned W = 8
) (
  input  logic         clk,
  input  logic         rst,    // async, active-high
  input  logic         load,
  input  logic         en,
  input  logic         up,     // 1 = count up, 0 = count down
  input  logic [W-1:0] din,
  output logic [W-1:0] q,
  output logic         tc      // terminal count
);

  always_ff @(posedge clk or posedge rst) begin
    if (rst)
      q <= '0;
    else if (load)
      q <= din;
    else if (en)
      q <= up ? (q + 1'b1) : (q - 1'b1);
    // else hold
  end

  // terminal count: all-ones when counting up, zero when counting down
  assign tc = up ? (&q) : (q == '0);

endmodule : counter
