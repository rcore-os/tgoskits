#version 450
layout(location=0) out vec4 o;
layout(push_constant) uniform PC { vec4 u; } pc;
void main(){ o=pc.u; }
