#version 450
layout(location=0) in vec2 p;
layout(location=1) in vec4 c;
layout(location=0) out vec4 vc;
void main(){ gl_Position=vec4(p,0.0,1.0); vc=c; }
