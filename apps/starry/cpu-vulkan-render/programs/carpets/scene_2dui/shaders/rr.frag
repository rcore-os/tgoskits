#version 450
layout(location=0) out vec4 o;
layout(push_constant) uniform PC { vec2 vp; vec4 col; vec4 box; float rad; } pc;
void main(){ vec2 p=gl_FragCoord.xy; float x0=pc.box.x,y0=pc.box.y,x1=pc.box.z,y1=pc.box.w; float rad=pc.rad;
  bool inside = p.x>=x0&&p.x<x1&&p.y>=y0&&p.y<y1;
  if(!inside){ discard; }
  vec2 c = p; bool corner=false; vec2 cc=vec2(0.0);
  if(p.x<x0+rad&&p.y<y0+rad){corner=true;cc=vec2(x0+rad,y0+rad);}
  else if(p.x>=x1-rad&&p.y<y0+rad){corner=true;cc=vec2(x1-rad,y0+rad);}
  else if(p.x<x0+rad&&p.y>=y1-rad){corner=true;cc=vec2(x0+rad,y1-rad);}
  else if(p.x>=x1-rad&&p.y>=y1-rad){corner=true;cc=vec2(x1-rad,y1-rad);}
  if(corner && distance(c,cc)>rad){ discard; }
  o=pc.col; }
