#version 450
layout(location=0) out vec4 o;
layout(push_constant) uniform PC { vec2 vp; vec2 col0; vec2 col1; vec2 tr; vec4 u; } pc;
void main(){ o=pc.u; }
