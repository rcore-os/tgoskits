#version 450
layout(location=0) in vec3 p;
layout(location=1) in vec3 c;
layout(location=0) out vec3 vc;
layout(push_constant) uniform PC { mat4 mvp; } pc;
void main(){ gl_Position=pc.mvp*vec4(p,1.0); vc=c; }
