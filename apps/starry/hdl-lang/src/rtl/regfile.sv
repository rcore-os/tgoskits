// =============================================================================
// regfile.sv -- synchronous-write / async-read register file.
//   Demonstrates: unpacked memory array, always_ff with reset, nonblocking
//   assignment, parameters derived from package, write-enable, dual read port,
//   for-loop reset, generate-free indexed access.
// IEEE 1800-2017: Cl.7.4 (unpacked arrays), Cl.9.2.2.4 (always_ff).
// =============================================================================
module regfile
  import hdl_pkg::*;
#(
  parameter int unsigned W  = DATA_W,
  parameter int unsigned N  = REGS,
  parameter int unsigned AW = ADDR_W
) (
  input  logic          clk,
  input  logic          rst,
  input  logic          we,
  input  logic [AW-1:0] waddr,
  input  logic [W-1:0]  wdata,
  input  logic [AW-1:0] raddr0,
  input  logic [AW-1:0] raddr1,
  output logic [W-1:0]  rdata0,
  output logic [W-1:0]  rdata1
);

  logic [W-1:0] mem [0:N-1];

  // async (combinational) read ports
  assign rdata0 = mem[raddr0];
  assign rdata1 = mem[raddr1];

  // synchronous write with synchronous reset clearing all entries
  always_ff @(posedge clk) begin
    if (rst) begin
      for (int i = 0; i < int'(N); i++)
        mem[i] <= '0;
    end else if (we) begin
      mem[waddr] <= wdata;
    end
  end

endmodule : regfile
