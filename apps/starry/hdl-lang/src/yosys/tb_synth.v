`timescale 1ns/1ps
module tb_synth;
  reg clk=0, rst=1, start=0, we=0;
  reg [7:0] a=0,b=0; reg [2:0] op=0; reg [3:0] waddr=0, raddr=0;
  wire [7:0] rdata; wire [1:0] phase; wire busy, zero;
  integer i, pass=0;
  datapath dut (.clk(clk),.rst(rst),.start(start),
     .a(a),.b(b),.op(op),.waddr(waddr),.raddr(raddr),.we(we),
     .rdata(rdata),.phase(phase),.busy(busy),.zero(zero));
  always #5 clk = ~clk;
  task step; begin @(posedge clk); #1; end endtask
  initial begin
    // reset
    rst=1; step; step; rst=0;
    // exercise ALU through datapath: write a few values into mem via we
    a=8'd6; b=8'd3; op=3'd0; we=1; waddr=4'd1; step; we=0;  // mem[1]=9
    a=8'd10; b=8'd4; op=3'd1; we=1; waddr=4'd2; step; we=0; // mem[2]=6
    a=8'hF0; b=8'h0F; op=3'd3; we=1; waddr=4'd3; step; we=0; // mem[3]=ff
    // read back
    raddr=4'd1; step; $display("SYN: rd1_xor_acc=%0d", rdata);
    raddr=4'd2; step; $display("SYN: rd2_xor_acc=%0d", rdata);
    raddr=4'd3; step; $display("SYN: rd3_xor_acc=%0d", rdata);
    // drive FSM: start->FETCH->EXEC->WB->IDLE
    op=3'd1; a=8'd5; b=8'd5; // a-b=0 -> aluz=1 -> done_in path
    start=1; step; start=0;
    for (i=0;i<6;i=i+1) begin $display("SYN: phase=%0d busy=%0d", phase, busy); step; end
    $display("SYN: zero_flag=%0d", zero);
    $display("SYN_DONE");
    $finish;
  end
endmodule
