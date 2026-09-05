#version 450
layout(location=0) in vec2 uv;
layout(location=0) out vec4 o;
layout(set=0,binding=0) uniform sampler2D yT;
layout(set=0,binding=1) uniform sampler2D uT;
layout(set=0,binding=2) uniform sampler2D vT;
void main(){ float Y=texture(yT,uv).r; float U=texture(uT,uv).r-0.5; float V=texture(vT,uv).r-0.5;
  float R=Y+1.402*V; float G=Y-0.344136*U-0.714136*V; float B=Y+1.772*U;
  o=vec4(clamp(vec3(R,G,B),0.0,1.0),1.0); }
