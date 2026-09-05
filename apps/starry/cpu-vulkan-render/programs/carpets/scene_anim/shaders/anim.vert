#version 450
layout(location=0) in vec2 lp;
layout(push_constant) uniform PC { vec2 vp; vec2 col0; vec2 col1; vec2 tr; vec4 u; } pc;
void main(){ vec2 pix = pc.col0*lp.x + pc.col1*lp.y + pc.tr; vec2 n=(pix/pc.vp)*2.0-1.0; gl_Position=vec4(n,0.0,1.0); }
