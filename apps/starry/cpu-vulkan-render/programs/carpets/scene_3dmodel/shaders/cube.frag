#version 450
layout(location=0) in vec3 vc;
layout(location=0) out vec4 o;
void main(){ o=vec4(vc,1.0); }
