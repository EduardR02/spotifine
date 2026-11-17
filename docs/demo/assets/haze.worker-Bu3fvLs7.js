(function(){"use strict";const Zt={peak:.1,mean:.045},lt=360,_e=.04045,Jt=12.92,Et=.055,te=2.4,Ee=.0031308,Rt=.2126,Ft=.7152,wt=.0722,ee=.001,oe=1e-4,pe=1e-5,it=1e-6,ve=t=>t<=_e?t/Jt:((t+Et)/(1+Et))**te,Xo=t=>t<=Ee?t*Jt:(1+Et)*t**(1/te)-Et,V=t=>Math.min(1,Math.max(0,t)),be=new Float32Array(4097);for(let t=0;t<=4096;t++)be[t]=ve(t/4096);const ne=t=>be[t>=1?4096:t<=0?0:t*4096+.5|0],H=new Float32Array(256);for(let t=0;t<256;t++)H[t]=ve(t/255);const It=new Float32Array(16385);for(let t=0;t<=16384;t++)It[t]=Xo(t/16384);function mt(t){if(t<=0)return 0;if(t>=1)return 1;const e=t*16384,o=e|0;return It[o]+(It[o+1]-It[o])*(e-o)}const Oe=(t,e)=>Rt*H[t[e]]+Ft*H[t[e+1]]+wt*H[t[e+2]],Pt=new Float64Array(8193);for(let t=0;t<=8192;t++)Pt[t]=(t/8192)**(4/3);function kt(t){if(!(t>=0&&t<=1))return Math.cbrt(t);const e=Math.sqrt(Math.sqrt(t))*8192,o=e|0;return o>=8192?1:Pt[o]+(Pt[o+1]-Pt[o])*(e-o)}const Gt=new Float64Array(4097);for(let t=0;t<=4096;t++)Gt[t]=(t/4096)**.6;function Le(t){if(t<=0)return 0;if(t>=1)return 1;const e=t*4096,o=e|0;return Gt[o]+(Gt[o+1]-Gt[o])*(e-o)}const Se=[.4122214708,.5363325363,.0514459929,.2119034982,.6806995451,.1073969566,.0883024619,.2817188376,.6299787005],Ce=[.2104542553,.793617785,-.0040720468,1.9779984951,-2.428592205,.4505937099,.0259040371,.7827717662,-.808675766],Te=[1,.3963377774,.2158037573,1,-.1055613458,-.0638541728,1,-.0894841775,-1.291485548],Re=[4.0767416621,-3.3077115913,.2309699292,-1.2684380046,2.6097574011,-.3413193965,-.0041960863,-.7034186147,1.707614701],[Yo,jo,$o,Vo,Qo,Zo,Jo,tn,en]=Se,[on,nn,an,rn,cn,sn,ln,un,fn]=Ce,[,hn,mn,,xn,Mn,,yn,dn]=Te,[An,gn,_n,En,pn,vn,bn,On,Ln]=Re;function pt(t,e,o,a){const r=kt(Yo*t+jo*e+$o*o),i=kt(Vo*t+Qo*e+Zo*o),c=kt(Jo*t+tn*e+en*o);return a[0]=on*r+nn*i+an*c,a[1]=rn*r+cn*i+sn*c,a[2]=ln*r+un*i+fn*c,a}function ae(t,e,o,a){const r=t+hn*e+mn*o,i=t+xn*e+Mn*o,c=t+yn*e+dn*o,s=r*r*r,n=i*i*i,u=c*c*c;return a[0]=An*s+gn*n+_n*u,a[1]=En*s+pn*n+vn*u,a[2]=bn*s+On*n+Ln*u,a}function Sn(t,e,o){return pt(ne(t),ne(e),ne(o),[0,0,0])}const nt=38,ut=156,Bt=54,re=138,Cn=(nt+ut)/2,Fe=t=>t-360*Math.floor(t/360);function Tn(t,e=Cn){const o=Fe(t);return o<nt||o>ut?o:o<e?nt+(o-nt)/(e-nt)*(Bt-nt):re+(o-e)/(ut-e)*(ut-re)}function Rn(t){return t>Bt?0:Math.max(0,(t-nt)/(Bt-nt))}const ce=.04,we=.5,Ie=.3,Pe=.12,ke=.22,Ge=.04,se=.05,Be=.13,ie=.018,vt=22,Ne=14,le=64,De=6;function Fn(t){const e=Fe(t);return e<vt||e>le?0:at(Math.min((e-vt)/Ne,(le-e)/De))}const He=[Math.cos(vt*Math.PI/180),Math.sin(vt*Math.PI/180)],Ue=[Math.cos(ut*Math.PI/180),Math.sin(ut*Math.PI/180)],ze=125,wn=.97,Nt=[0,0,0],ue=.13,In=.3,Ke=.58,qe=1.3,We=1.5,Xe=.2,Ye=.05,je=.7,$e=.12,fe=.07,Ve=.9,Qe=.62,Ze=.33,Je=.6,ft=1e-4,he=.88,to=.995,eo=16,Pn=.05,oo=.8,me=.03,no=.6,ao=1.4,ro=.2,co=.2,so=4.8,io=Math.cos(so),lo=Math.sin(so);function uo(t,e,o){return t+ro*Math.sqrt(e*e+o*o)+co*Math.max(0,e*io+o*lo)}function kn(t,e,o){return{lo:Math.min(t,e-.1),mid:e,hi:Math.max(o+.02,e+.15),M:In-.05*at((.3-e)/.25)}}function Gn(t,e){return t<=e.mid?ue+(e.M-ue)*V((t-e.lo)/Math.max(ee,e.mid-e.lo)):e.M+(Ke-e.M)*V((t-e.mid)/Math.max(ee,e.hi-e.mid))**qe}function Bn(t,e,o,a,r,i,c,s){let n=Math.sqrt(e*e+o*o);const u=n,x=ce*Math.min(1,Math.max(Ie,t/we)),g=V(1-n/x);if(g>0){const m=Math.sqrt(r[1]*r[1]+r[2]*r[2]);if(m>oe){const f=Math.min(m,Pe);e+=g*(r[1]/m*f-e),o+=g*(r[2]/m*f-o),n=Math.sqrt(e*e+o*o)}}let M=0,l=-1;if(He[0]*o-He[1]*e>=0&&e*Ue[1]-o*Ue[0]>=0&&(l=Math.atan2(o,e)*180/Math.PI,n>oe&&l>=nt)){l=Tn(l,ze),M=Rn(l);const m=n*(1-ke*M);e=m*Math.cos(l*Math.PI/180),o=m*Math.sin(l*Math.PI/180),n=m}const h=(Gn(t,a)+Ge*M)*i,y=1+je*(1-at(u/$e)),_=n>it?Math.min(n*We*y*Math.sqrt(h/Math.max(t,Ye)),Xe)/n:0;e*=_,o*=_;const A=_>0&&l>=0?Fn(l):0;if(A>0){const m=Math.sqrt(e*e+o*o),f=at((u-se)/(Be-se)),d=Math.min(m,ie+(m-ie)*f),E=m>it?(m+A*(d-m))/m:1;e*=E,o*=E}return fo(h,e,o,i,c,s)}function fo(t,e,o,a,r,i){const c=Qe*a,s=uo(t,e,o);if(s>c){const n=Math.sqrt(e*e+o*o),u=(s-t)/Math.max(n,it),x=Math.max(n*Je,Math.min(n,(c-t)/u));e*=x/Math.max(n,it),o*=x/Math.max(n,it),t=Math.min(t,c-u*x)}for(let n=0;n<eo;n++){ae(t,e,o,Nt);const u=Nt[0],x=Nt[1],g=Nt[2];if(u<-ft||x<-ft||g<-ft||u>1+ft||x>1+ft||g>1+ft){e*=he,o*=he;continue}const M=Rt*u+Ft*x+wt*g;if(M>r){t*=Math.cbrt(r/M)*to;continue}break}return i[0]=t,i[1]=e,i[2]=o,i}function Nn(t,e,o,a=[0,0,e,o]){const i=(h,y)=>{const _=(y*e+h)*4;return t[_]<20&&t[_+1]<20&&t[_+2]<20};let[c,s,n,u]=a;const x=n-c,g=u-s,M=h=>{for(let y=c;y<n;y++)if(!i(y,h))return!1;return!0},l=h=>{for(let y=s;y<u;y++)if(!i(h,y))return!1;return!0};for(;s<a[1]+g*.4&&M(s);)s++;for(;u>a[3]-g*.4&&M(u-1);)u--;for(;c<a[0]+x*.4&&l(c);)c++;for(;n>a[2]-x*.4&&l(n-1);)n--;return[c,s,n,u]}function Dt(t,e){const{w:o,h:a}=t,r=Io(e),i=r?r.w/r.h:null;let c=[0,0,o,a];if(i)if(o/a>i){const s=Math.max(1,Math.round(a*i)),n=Math.floor((o-s)/2);c=[n,0,n+s,a]}else{const s=Math.max(1,Math.round(o/i)),n=Math.floor((a-s)/2);c=[0,n,o,n+s]}return{shown:c,rect:Nn(t.data,o,a,c)}}function Dn(t){const e=t.data;for(let o=0;o<e.length;o+=4)if(Oe(e,o)>.012)return!1;return!0}const ho=2,bt=4096;function Hn(t,e){const[o,a,r,i]=e,c=t.data,s=new Uint32Array(bt),n=[0,0,0],u=(ce*.5)**2;let x=0,g=0;for(let l=a;l<i;l+=ho)for(let h=o;h<r;h+=ho){const y=(l*t.w+h)*4;pt(H[c[y]],H[c[y+1]],H[c[y+2]],n),s[Math.min(bt-1,Math.max(0,n[0]*bt|0))]++,n[1]*n[1]+n[2]*n[2]<u&&g++,x++}const M=l=>{const h=Math.min(x-1,Math.floor(l*x));let y=0;for(let _=0;_<bt;_++){if(y+s[_]>h)return(_+(h-y+.5)/s[_])/bt;y+=s[_]}return 1};return{ex:kn(M(.05),M(.5),M(.97)),grey:g/Math.max(1,x)}}const Un=.18,zn=.58,Ht=[.07,.17];function mo(t){let e=0,o=0,a=0;for(let i=0;i<t.length;i+=4)e+=t[i],o+=t[i+1],a+=t[i+2];const r=Math.max(1,t.length/4);return[Math.round(e/r),Math.round(o/r),Math.round(a/r)]}function Kn(t,e,o){const a=new Uint8Array(Ht.length*o*4);let r=0;for(const i of Ht){const c=Math.min(e-1,Math.floor(e*i));for(let s=0;s<o;s++){const n=(s*e+c)*4;a[r++]=t[n],a[r++]=t[n+1],a[r++]=t[n+2],a[r++]=t[n+3]}}return a}function xt(t,e,o=!1){const[a,r,i,c]=e,s=c-r,n=r+Math.max(1,Math.round(s*Un)),u=r+Math.round(s*zn),x=(g,M)=>{const l=new Float32Array((M-g)*(i-a));let h=0;for(let y=g;y<M;y++){const _=(o?t.h-1-y:y)*t.w;for(let A=a;A<i;A++)l[h++]=Oe(t.data,(_+A)*4)}return l.length?Wn(qn(l,Math.min(l.length-1,Math.floor(l.length*.9)))):0};return{top:x(r,n),bottom:x(u,c)}}function qn(t,e){let o=0,a=t.length-1;for(;o<a;){const r=t[o+a>>1];let i=o,c=a;for(;i<=c;){for(;t[i]<r;)i++;for(;t[c]>r;)c--;if(i<=c){const s=t[i];t[i]=t[c],t[c]=s,i++,c--}}if(e<=c)a=c;else if(e>=i)o=i;else break}return t[e]}const Wn=t=>Math.round(t*1e3)/1e3,rt={x:3,y:5};function Xn(t,e){const[o,a,r,i]=e,c=new Float32Array(rt.x*rt.y*3),s=[0,0,0];for(let n=0;n<rt.y;n++)for(let u=0;u<rt.x;u++){const x=Math.floor(o+(r-o)*u/rt.x),g=Math.max(x+1,Math.floor(o+(r-o)*(u+1)/rt.x)),M=Math.floor(a+(i-a)*n/rt.y),l=Math.max(M+1,Math.floor(a+(i-a)*(n+1)/rt.y));let h=0,y=0,_=0,A=0;for(let m=M;m<l;m++)for(let f=x;f<g;f++){const d=(m*t.w+f)*4;h+=H[t.data[d]],y+=H[t.data[d+1]],_+=H[t.data[d+2]],A++}pt(h/A,y/A,_/A,s),c.set(s,(n*rt.x+u)*3)}return c}function xo(t,e){let o=0;for(let a=0;a<t.length;a+=3)o+=Math.hypot(t[a]-e[a],t[a+1]-e[a+1],t[a+2]-e[a+2]);return o/(t.length/3)}function Yn(t){let e=0,o=1/0;for(let a=0;a<t.length;a++){let r=0;for(let i=0;i<t.length;i++)a!==i&&(r+=xo(t[a],t[i]));r<o&&(o=r,e=a)}return e}const jn=.08,Mo=t=>Math.max(1,Math.round(t-.5));function yo(t,e,o,a,r,i){const c=1/(2*r+1),s=o*i;for(let n=0;n<a;n++){const u=n*s;for(let x=0;x<i;x++){const g=u+x,M=g+(o-1)*i;let l=0;for(let h=-r;h<=r;h++)l+=t[g+(h<0?0:h>o-1?o-1:h)*i];for(let h=g,y=g+(r+1)*i,_=g-r*i;h<=M;h+=i,y+=i,_+=i)e[h]=l*c,l+=t[y<M?y:M]-t[_>g?_:g]}}}function Ao(t,e,o,a,r,i,c){const s=1/(2*r+1),n=o*i;c.fill(0,0,n);for(let u=-r;u<=r;u++){const x=(u<0?0:u>a-1?a-1:u)*n;for(let g=0;g<n;g++)c[g]+=t[x+g]}for(let u=0;u<a;u++){const x=u*n,g=(u+r+1<a?u+r+1:a-1)*n,M=(u-r>0?u-r:0)*n;for(let l=0;l<n;l++)e[x+l]=c[l]*s,c[l]+=t[g+l]-t[M+l]}}function Ot(t,e,o,a,r=3){const i=Mo(a),c=new Float32Array(t.length),s=new Float32Array(t.length),n=new Float64Array(e*r);yo(t,s,e,o,i,r),Ao(s,c,e,o,i,r,n);for(let u=1;u<3;u++)yo(c,s,e,o,i,r),Ao(s,c,e,o,i,r,n);return c}const $n=6,go=3,Vn=18,_o=2.4,Qn=.55,Zn=.2,Jn=1.2,Eo=.6,po=.2,vo=1.3,ta=.44,bo=.6,Oo=.85,Lo=2.2,ea=.06,oa=1.05,Ut=.5,So=.72,zt=44,Lt=22,Co=.5,To=1.5,Kt=.9,na=10,xe=8,Me=16,Mt=.213,yt=.715,dt=.072,Ro=2146121005,Fo=2221713035,qt=24;function ye(t){return t^=t>>>16,t=Math.imul(t,Ro),t^=t>>>15,t=Math.imul(t,Fo),t^=t>>>16,t>>>0}const wo=t=>[ye(2*t),ye(2*t+1)],At=(t,e)=>(ye((t^e)>>>0)>>>32-qt)/2**qt,Wt={floats:{SRGB_CUT:_e,SRGB_SLOPE:Jt,SRGB_OFFSET:Et,SRGB_GAMMA:te,SRGB_LINEAR_CUT:Ee,LUMA_R:Rt,LUMA_G:Ft,LUMA_B:wt,EPS3:ee,EPS4:oe,EPS5:pe,EPS6:it,ARC_LO:nt,ARC_HI:ut,WARM_TIP:Bt,GREEN_TOE:re,HAZE_FOLD:ze,C_GREY:ce,GREY_L:we,GREY_FLOOR:Ie,TINT_CMAX:Pe,CLAY_SOFTEN:ke,CLAY_LIFT:Ge,WARM_PALE:se,WARM_RICH:Be,WARM_BREATH:ie,WARM_FROM:vt,WARM_IN:Ne,WARM_TO:le,WARM_OUT:De,G_SHADOW:ue,G_LIGHT:Ke,G_HI_GAMMA:qe,G_CHROMA:We,G_CMAX:Xe,L_FLOOR:Ye,G_VIBRANCE:je,VIB_KNEE:$e,C_MEAN:fe,C_BUDGET_EXP:Ve,P_PEAK:Qe,P_MEAN:Ze,CAP_KEEP:Je,GAMUT_TOL:ft,GAMUT_STEP:he,Y_MARGIN:to,LOCAL_GAIN:oo,DETAIL_FLOOR:me,DETAIL_KMIN:no,DETAIL_KMAX:ao,HK_C:ro,HK_BLUE:co,HK_A:io,HK_B:lo,FALL_CHROMA:Eo,POOL:po,POOL_EXP:vo,POOL_REACH:bo,GIVE_BACK:Oo,GIVE_MAX:Lo,BAR_KNEE:Ut,BAR_TAIL:So,SEAM:zt,SEAM_IN:Lt,SEAM_HOLD:Co,SEAM_DIM:Kt,GAP:xe,SPILL:Me,MEAN:Zt.mean,NOISE_SCALE:2**qt},ints:{CAP_ITER:eo,NOISE_SHIFT:32-qt},uints:{HASH_M1:Ro,HASH_M2:Fo},mat3:{LMS_OF_RGB:Se,LAB_OF_LMS:Ce,LMS_OF_LAB:Te,RGB_OF_LMS:Re}},at=t=>t<=0?0:t>=1?1:t*t*(3-2*t),de=t=>{const e=Math.abs(t),o=e-2*Math.floor(e/2);return o<=1?o:2-o};function aa(t,e,o,a,r,i,c,s){const n=Math.min(e-1,Math.max(0,r*e-.5)),u=Math.min(o-1,Math.max(0,i*o-.5)),x=Math.max(0,Math.min(e-2,Math.floor(n))),g=Math.max(0,Math.min(o-2,Math.floor(u))),M=Math.min(e-1,x+1),l=Math.min(o-1,g+1),h=Math.min(1,Math.max(0,n-x)),y=Math.min(1,Math.max(0,u-g)),_=(g*e+x)*a,A=(g*e+M)*a,m=(l*e+x)*a,f=(l*e+M)*a;for(let d=0;d<a;d++){const E=t[_+d]+(t[A+d]-t[_+d])*h,C=t[m+d]+(t[f+d]-t[m+d])*h;c[s+d]=E+(C-E)*y}}function ra(t,e,o,a,r,i=e){const c=i-a,s=new Float32Array(c*o*r);for(let n=0;n<o;n++)s.set(t.subarray((n*e+a)*r,(n*e+i)*r),n*c*r);return s}function Io(t){if(t.left==null)return null;if(t.video)return t.video;const e=t.left*t.w;return{x:e,y:0,w:t.w-e,h:t.h}}function ca(t,e){const o=new Float32Array(e*4),a=Qn*Math.max(1,t.x);let r=0,i=0;for(let c=e-1;c>=0;c--){const s=t.x-(c+.5),n=c*4;if(s<=0){o[n]=-s/t.w,o[n+1]=o[n+2]=o[n+3]=1;continue}r+=(s-i)/(t.w*(1+(_o-1)*at((s+i)/2/a))),i=s,o[n]=r,o[n+1]=1+(_o-1)*at(s/a),o[n+2]=1-Zn*(s/Math.max(1,t.x))**Jn,o[n+3]=o[n+2]**Eo}return o}function Po(t,e,o,a=1){const{w:r,h:i}=e,c=e.px??5,s=Io(e),n=!!e.immersive,u=s&&e.video&&n?s:null,x=!!s&&!n,{shown:g,rect:M}=Dt(t,e),l=M[2]-M[0],h=M[3]-M[1],{ex:y,grey:_}=Hn(t,M),A=$n/c,m=Math.max(r/l,i/h);let f=null;if(u){const d=na/(s.w*c/l);f={x0:Math.max(0,Math.floor(s.x-zt/c)),x1:Math.min(r,Math.ceil(s.x+To*Lt/c)),soft:d,ew:Math.min(l,Math.ceil((zt+To*Lt)/c/s.w*l+3*d)+2)}}return{w:r,h:i,px:c,frame:s,video:u,cover:x,immersive:n,shown:g,rect:M,rw:l,rh:h,ex:y,grey:_,seam:f,tintLab:Sn(...o),scale:a,s3:Math.cbrt(a),ycap:Zt.peak*a*wn,local:Pn*Math.max(l,h),lens:A,softer:A*go,softX0:x?Math.max(0,Math.floor(s.x-3*A*go)):0,corner:Vn/c,measureTo:s&&n?Math.max(1,Math.floor(s.x)):r,cols:s?ca(s,r):null,pool:x?sa(s):null,closed:s?null:{k:m,ox:(r-l*m)/2,oy:(i-h*m)/2,cx:r*ea,radius:r*oa}}}function sa(t){const e=t.y+t.h*ta;return{cy:e,span:Math.max(1,e-t.y,t.y+t.h-e)}}function ia(t,e){return 1-at((t.x-(e+.5))/(bo*Math.max(1,t.x)))}function la(t,e){return Math.min(1,Math.abs(e+.5-t.cy)/t.span)**vo}function ua(t,e,o,a){const r=e+.5-t.cx,i=o+.5-a,c=Math.sqrt(r*r+i*i)/t.radius;return 1-(c<Ut?c:Ut+(c-Ut)*So)}function fa(t,e,o,{scale:a=1,seed:r=1,frost:i=null}={}){const c=Po(t,e,o,a),{w:s,h:n,frame:u,cols:x,closed:g,s3:M,ycap:l}=c,h=[0,0,0],y=[0,0,0],_=ha(t,c),{rw:A,rh:m}=c,f=new Float32Array(s*n*4),d=new Int32Array(s),E=new Int32Array(s),C=new Float32Array(s);for(let v=0;v<s;v++){const P=u?de(x[v*4]):(v+.5-g.ox)/(A*g.k),p=Math.min(A-1,Math.max(0,P*A-.5));d[v]=Math.max(0,Math.min(A-2,Math.floor(p))),E[v]=Math.min(A-1,d[v]+1),C[v]=Math.min(1,Math.max(0,p-d[v]))}for(let v=0;v<n;v++){const P=u?0:(v+.5-g.oy)/(m*g.k);for(let p=0;p<s;p++){const k=u?de(.5+((v+.5-u.y)/u.h-.5)/x[p*4+1]):P,q=Math.min(m-1,Math.max(0,k*m-.5)),Y=Math.max(0,Math.min(m-2,Math.floor(q))),$=Math.min(m-1,Y+1),O=Math.min(1,Math.max(0,q-Y)),b=C[p],N=(Y*A+d[p])*4,ct=(Y*A+E[p])*4,D=($*A+d[p])*4,st=($*A+E[p])*4,W=(v*s+p)*4;for(let L=0;L<4;L++){const tt=_[N+L]+(_[ct+L]-_[N+L])*b,Qt=_[D+L]+(_[st+L]-_[D+L])*b;f[W+L]=tt+(Qt-tt)*O}}}const S=Ot(f,s,n,c.lens,4),w=c.softX0,U=s-w,T=c.cover?Ot(ra(f,s,n,w,4),U,n,c.softer,4):null,j=c.corner,R=(v,P)=>{if(u.x-(v+.5)>0)return 0;const p=u.x+j-(v+.5),k=Math.max(j-(P+.5),P+.5-(n-j));return p<=0||k<=0?1:V(j-Math.sqrt(p*p+k*k)+.5)},et=c.measureTo,G=c.pool?Float32Array.from({length:s},(v,P)=>ia(u,P)):null;let X=0,z=0;for(let v=0;v<n;v++){const P=c.pool?po*la(c.pool,v):0;for(let p=0;p<s;p++){const k=(v*s+p)*4;let q=S[k],Y=S[k+1],$=S[k+2],O=S[k+3];const b=T?R(p,v):0;if(b>0){const L=(v*U+p-w)*4;q+=(T[L]-q)*b,Y+=(T[L+1]-Y)*b,$+=(T[L+2]-$)*b,O+=(T[L+3]-O)*b}pt(q,Y,$,h);const N=Math.sqrt(h[1]*h[1]+h[2]*h[2]),ct=N>pe?Math.min(Lo,Math.max(1,O*Oo/N)):1;let D,st;G?(D=x[p*4+2]*(1-P*G[p]),st=Le(D)):u?(D=x[p*4+2],st=x[p*4+3]):(D=ua(g,p,v,n),st=Le(D));const W=ct*st;S[k]=h[0]*D,S[k+1]=h[1]*W,S[k+2]=h[2]*W,p<et&&(X+=N*W,z++)}}const K=X/Math.max(1,z),Q=K>fe*M?(fe*M/K)**Ve:1,B=f;let Z=0,ot=0,J=0;for(let v=0;v<n;v++)for(let P=0;P<s;P++){const p=(v*s+P)*4,k=S[p],q=S[p+1]*Q,Y=S[p+2]*Q;ae(k,q,Y,y);let $=V(y[0]),O=V(y[1]),b=V(y[2]),N=Rt*$+Ft*O+wt*b,ct=1;if(N>l){const D=l/N;$*=D,O*=D,b*=D,N=l,ct=kt(D)}B[p]=$,B[p+1]=O,B[p+2]=b,P<et&&(Z+=N,ot+=uo(k,q,Y)*ct,J++)}const qo=Z/Math.max(1,J),Vt=ot/Math.max(1,J),ge=Math.min(1,Zt.mean*a/Math.max(it,qo),(Ze*M/Math.max(it,Vt))**3);return xa(B,ge,c,t,r,i,c.seam?ma(t,c):null)}function ha(t,e){const{rw:o,rh:a,ex:r,tintLab:i,s3:c,ycap:s}=e,[n,u]=e.rect,x=t.data,g=o*a,M=[0,0,0],l=new Float32Array(g*3),h=new Float32Array(g*2);for(let m=0;m<a;m++)for(let f=0;f<o;f++){const d=((u+m)*t.w+n+f)*4,E=m*o+f;pt(H[x[d]],H[x[d+1]],H[x[d+2]],M);const C=M[0];Bn(C,M[1],M[2],r,i,c,s,M),l[E*3]=M[0],l[E*3+1]=M[1],l[E*3+2]=M[2],h[E*2]=C,h[E*2+1]=M[0]}const y=Ot(h,o,a,e.local,2),_=new Float32Array(g*4),A=[0,0,0];for(let m=0;m<g;m++){const f=Math.max(me,y[m*2+1]+oo*(h[m*2]-y[m*2])),d=Math.min(ao,Math.max(no,Math.sqrt(f/Math.max(l[m*3],me))));fo(f,l[m*3+1]*d,l[m*3+2]*d,c,s,M),_[m*4+3]=Math.sqrt(M[1]*M[1]+M[2]*M[2]),ae(M[0],M[1],M[2],A),_[m*4]=V(A[0]),_[m*4+1]=V(A[1]),_[m*4+2]=V(A[2])}return _}function ma(t,e){const{h:o,px:a,frame:r,rw:i,rh:c}=e,{x0:s,x1:n,soft:u,ew:x}=e.seam,[g,M]=e.rect,l=new Float32Array(x*c*3);for(let m=0;m<c;m++)for(let f=0;f<x;f++){const d=((M+m)*t.w+g+f)*4,E=(m*x+f)*3;l[E]=H[t.data[d]],l[E+1]=H[t.data[d+1]],l[E+2]=H[t.data[d+2]]}const h=Ot(l,x,c,u,3),y=Math.max(0,n-s),_=new Float32Array(y*o*3),A=new Float32Array(y*o);for(let m=0;m<o;m++){const f=de((m+.5-r.y)/r.h);for(let d=s;d<n;d++){const E=(r.x-(d+.5))*a,C=E>0?1-at(E/zt):1-at((-E-Co*Lt)/Lt);if(C<=0)continue;const S=m*y+(d-s);aa(h,x,c,3,Math.abs(E)/a/r.w*i/x,f,_,S*3),_[S*3]*=Kt,_[S*3+1]*=Kt,_[S*3+2]*=Kt,A[S]=C}}return{x0:s,w:y,light:_,alpha:A}}function xa(t,e,o,a,r,i,c){const{w:s,h:n,px:u,frame:x,shown:g}=o,[M,l]=wo(r),h=new Uint8ClampedArray(s*n*4),y=i?new Float32Array(s*n*3):null,_=i?Math.ceil(3*i.sigma)+2:0,A=i&&c?Math.max(0,Math.floor(x.x-(xe+Me)/u-_)&-2):0,m=i&&c?Math.min(s,c.x0+c.w+_):0,f=m-A,d=f>0?new Float32Array(f*n*3):null;for(let U=0;U<n;U++)for(let T=0;T<s;T++){const j=U*s+T,R=j*4,et=j*3;let G=t[R]*e,X=t[R+1]*e,z=t[R+2]*e,K=mt(G),Q=mt(X),B=mt(z);y&&(y[et]=K,y[et+1]=Q,y[et+2]=B);const Z=c&&T>=c.x0&&T<c.x0+c.w?U*c.w+(T-c.x0):-1,ot=Z>=0?c.alpha[Z]:0;if(ot>0&&(G+=(c.light[Z*3]-G)*ot,X+=(c.light[Z*3+1]-X)*ot,z+=(c.light[Z*3+2]-z)*ot,K=mt(G),Q=mt(X),B=mt(z)),d&&T>=A&&T<m){const J=(U*f+T-A)*3;d[J]=K,d[J+1]=Q,d[J+2]=B}h[R]=Math.floor(K*255+At(R,M)),h[R+1]=Math.floor(Q*255+At(R+1,M)),h[R+2]=Math.floor(B*255+At(R+2,M)),h[R+3]=255}const E=x&&!o.immersive?xt({data:h,w:s,h:n},[Math.min(s-1,Math.ceil(x.x)),0,s,n]):xt(a,g),C=Math.ceil(s/2),S=Math.ceil(n/2);let w=null;if(i){const U={...i,sigma:i.sigma/2},T=Go(ko(y,s,n),C,S,U);if(d){const j=Math.ceil(f/2),R=A/2,et=Go(ko(d,f,n),j,S,U);for(let G=0;G<j&&R+G<C;G++){const X=Ma(x,u,R+G);if(!(X<=0))for(let z=0;z<S;z++){const K=(z*C+R+G)*3,Q=(z*j+G)*3;for(let B=0;B<3;B++)T[K+B]+=(et[Q+B]-T[K+B])*X}}}w=da(T,C,S,l)}return{rgba:h,frost:w,frostW:C,frostH:S,tone:w?mo(Kn(w,C,S)):null,gather:w&&i.edge?ya(w,i.edge):null,gain:e,grey:o.grey,light:E}}function Ma(t,e,o){const a=(t.x-2*(o+.5))*e-xe;return 1-at(a/Me)}function ko(t,e,o){const a=Math.ceil(e/2),r=Math.ceil(o/2),i=new Float32Array(a*r*3);for(let c=0;c<r;c++){const s=2*c,n=Math.min(o-1,s+1);for(let u=0;u<a;u++){const x=2*u,g=Math.min(e-1,x+1),M=(c*a+u)*3;for(let l=0;l<3;l++)i[M+l]=(t[(s*e+x)*3+l]+t[(s*e+g)*3+l]+t[(n*e+x)*3+l]+t[(n*e+g)*3+l])/4}}return i}function ya(t,e){const o=new Uint8ClampedArray(t.length),a=Xt(e.saturate),r=e.gain,i=e.alpha,c=e.tint,s=[i*c[0],i*c[1],i*c[2]],n=u=>u<0?0:u>1?1:u;for(let u=0;u<t.length;u+=4){const x=n(((1-i)*t[u]/255+s[0])*r),g=n(((1-i)*t[u+1]/255+s[1])*r),M=n(((1-i)*t[u+2]/255+s[2])*r);for(let l=0;l<3;l++){const h=n(a[l*3]*x+a[l*3+1]*g+a[l*3+2]*M);o[u+l]=Math.round(n((h-s[l])/(1-i))*255)}o[u+3]=255}return o}function Xt(t){return[Mt+(1-Mt)*t,yt-yt*t,dt-dt*t,Mt-Mt*t,yt+(1-yt)*t,dt-dt*t,Mt-Mt*t,yt-yt*t,dt+(1-dt)*t]}function Go(t,e,o,a){const r=Ot(t,e,o,a.sigma),i=Xt(a.saturate),c=a.brightness;for(let s=0;s<e*o;s++){const n=s*3,u=r[n],x=r[n+1],g=r[n+2];for(let M=0;M<3;M++)r[n+M]=V(V(i[M*3]*u+i[M*3+1]*x+i[M*3+2]*g)*c)}return r}function da(t,e,o,a){const r=new Uint8ClampedArray(e*o*4);for(let i=0;i<e*o;i++){const c=i*3,s=i*4;r[s]=Math.floor(t[c]*255+At(s,a)),r[s+1]=Math.floor(t[c+1]*255+At(s+1,a)),r[s+2]=Math.floor(t[c+2]*255+At(s+2,a)),r[s+3]=255}return r}function Aa(t){const e=new Uint8ClampedArray(64);for(let o=0;o<e.length;o+=4)e[o]=Math.round(t[0]*255),e[o+1]=Math.round(t[1]*255),e[o+2]=Math.round(t[2]*255),e[o+3]=255;return{data:e,w:4,h:4}}const St=8,Bo=t=>{const e=String(t);return/[.e]/.test(e)?e:`${e}.0`};function ga(){const t=[];for(const[e,o]of Object.entries(Wt.floats))t.push(`#define ${e} ${Bo(o)}`);for(const[e,o]of Object.entries(Wt.ints))t.push(`#define ${e} ${o}`);for(const[e,o]of Object.entries(Wt.uints))t.push(`#define ${e} ${o>>>0}u`);for(const[e,o]of Object.entries(Wt.mat3)){const a=[0,1,2].flatMap(r=>[o[r],o[3+r],o[6+r]]);t.push(`#define ${e} mat3(${a.map(Bo).join(", ")})`)}return t.push(`#define BLOCK ${St}`),t.join(`
`)}const _a=`#version 300 es
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`,Ea=`
out vec4 outColor;

float clamp01(float v) { return clamp(v, 0.0, 1.0); }
float smooth01(float t) { t = clamp01(t); return t * t * (3.0 - 2.0 * t); }
float linOf(float c) { return c <= SRGB_CUT ? c / SRGB_SLOPE : pow((c + SRGB_OFFSET) / (1.0 + SRGB_OFFSET), SRGB_GAMMA); }
vec3 linOf3(vec3 c) { return vec3(linOf(c.r), linOf(c.g), linOf(c.b)); }
float encOf(float y) {
  y = clamp01(y);
  return y <= SRGB_LINEAR_CUT ? y * SRGB_SLOPE : (1.0 + SRGB_OFFSET) * pow(y, 1.0 / SRGB_GAMMA) - SRGB_OFFSET;
}
vec3 encOf3(vec3 c) { return vec3(encOf(c.r), encOf(c.g), encOf(c.b)); }
float cbrt(float x) { return sign(x) * pow(abs(x), 1.0 / 3.0); }
vec3 labOfLinear(vec3 c) {
  vec3 lms = LMS_OF_RGB * c;
  return LAB_OF_LMS * vec3(cbrt(lms.x), cbrt(lms.y), cbrt(lms.z));
}
vec3 linearOfLab(vec3 lab) {
  vec3 lms = LMS_OF_LAB * lab;
  return RGB_OF_LMS * (lms * lms * lms);
}
float lumOf(vec3 c) { return dot(c, vec3(LUMA_R, LUMA_G, LUMA_B)); }
float perceived(vec3 lab) { return lab.x + HK_C * length(lab.yz) + HK_BLUE * max(0.0, lab.y * HK_A + lab.z * HK_B); }
float mirror(float t) {
  float a = abs(t), m = a - 2.0 * floor(a / 2.0);
  return m <= 1.0 ? m : 2.0 - m;
}
uint hash32(uint v) {
  v ^= v >> 16;
  v *= HASH_M1;
  v ^= v >> 15;
  v *= HASH_M2;
  v ^= v >> 16;
  return v;
}
float noiseAt(int i, uint key) { return float(hash32(uint(i) ^ key) >> NOISE_SHIFT) / NOISE_SCALE; }
/* A bilinear read at (u, v) in 0..1, clamped, exactly as haze.js samples. */
vec4 bilinear(sampler2D tex, float u, float v) {
  ivec2 size = textureSize(tex, 0);
  float iw = float(size.x), ih = float(size.y);
  float sx = min(iw - 1.0, max(0.0, u * iw - 0.5));
  float sy = min(ih - 1.0, max(0.0, v * ih - 0.5));
  int x0 = max(0, min(size.x - 2, int(floor(sx)))), y0 = max(0, min(size.y - 2, int(floor(sy))));
  int x1 = min(size.x - 1, x0 + 1), y1 = min(size.y - 1, y0 + 1);
  float tx = clamp01(sx - float(x0)), ty = clamp01(sy - float(y0));
  vec4 a = texelFetch(tex, ivec2(x0, y0), 0), b = texelFetch(tex, ivec2(x1, y0), 0);
  vec4 c = texelFetch(tex, ivec2(x0, y1), 0), d = texelFetch(tex, ivec2(x1, y1), 0);
  vec4 top = a + (b - a) * tx, bot = c + (d - c) * tx;
  return top + (bot - top) * ty;
}
`,No=`
uniform vec4 uEx;
uniform vec3 uTint;
uniform float uS3;
uniform float uYcap;

vec3 capInto(float Lo, float A, float B) {
  float cap = P_PEAK * uS3;
  float p = perceived(vec3(Lo, A, B));
  if (p > cap) {
    float c = sqrt(A * A + B * B);
    float kc = (p - Lo) / max(c, EPS6);
    float c2 = max(c * CAP_KEEP, min(c, (cap - Lo) / kc));
    A *= c2 / max(c, EPS6);
    B *= c2 / max(c, EPS6);
    Lo = min(Lo, cap - kc * c2);
  }
  for (int i = 0; i < CAP_ITER; i++) {
    vec3 rgb = linearOfLab(vec3(Lo, A, B));
    if (any(lessThan(rgb, vec3(-GAMUT_TOL))) || any(greaterThan(rgb, vec3(1.0 + GAMUT_TOL)))) {
      A *= GAMUT_STEP;
      B *= GAMUT_STEP;
      continue;
    }
    float y = lumOf(rgb);
    if (y > uYcap) {
      Lo *= cbrt(uYcap / y) * Y_MARGIN;
      continue;
    }
    break;
  }
  return vec3(Lo, A, B);
}

float keyOf(float L) {
  if (L <= uEx.y) return G_SHADOW + (uEx.w - G_SHADOW) * clamp01((L - uEx.x) / max(EPS3, uEx.y - uEx.x));
  return uEx.w + (G_LIGHT - uEx.w) * pow(clamp01((L - uEx.y) / max(EPS3, uEx.z - uEx.y)), G_HI_GAMMA);
}

vec3 gradeInto(vec3 lab) {
  float L = lab.x, A = lab.y, B = lab.z;
  float C = sqrt(A * A + B * B);
  float source = C;
  float grey = C_GREY * min(1.0, max(GREY_FLOOR, L / GREY_L));
  float tg = clamp01(1.0 - C / grey);
  if (tg > 0.0) {
    float tc = length(uTint.yz);
    if (tc > EPS4) {
      float target = min(tc, TINT_CMAX);
      A += tg * (uTint.y / tc * target - A);
      B += tg * (uTint.z / tc * target - B);
      C = sqrt(A * A + B * B);
    }
  }
  float clay = 0.0;
  float hue = -1.0;
  vec2 warmEdge = vec2(cos(radians(WARM_FROM)), sin(radians(WARM_FROM)));
  vec2 arcEdge = vec2(cos(radians(ARC_HI)), sin(radians(ARC_HI)));
  if (warmEdge.x * B - warmEdge.y * A >= 0.0 && A * arcEdge.y - B * arcEdge.x >= 0.0) {
    hue = (A == 0.0 && B == 0.0) ? 0.0 : degrees(atan(B, A));
    if (C > EPS4 && hue >= ARC_LO) {
      if (hue <= ARC_HI) {
        hue = hue < HAZE_FOLD
          ? ARC_LO + (hue - ARC_LO) / (HAZE_FOLD - ARC_LO) * (WARM_TIP - ARC_LO)
          : GREEN_TOE + (hue - HAZE_FOLD) / (ARC_HI - HAZE_FOLD) * (ARC_HI - GREEN_TOE);
      }
      clay = hue > WARM_TIP ? 0.0 : max(0.0, (hue - ARC_LO) / (WARM_TIP - ARC_LO));
      float c = C * (1.0 - CLAY_SOFTEN * clay);
      A = c * cos(radians(hue));
      B = c * sin(radians(hue));
      C = c;
    }
  }
  float Lo = (keyOf(L) + CLAY_LIFT * clay) * uS3;
  float vib = 1.0 + G_VIBRANCE * (1.0 - smooth01(source / VIB_KNEE));
  float k = C > EPS6 ? min(C * G_CHROMA * vib * sqrt(Lo / max(L, L_FLOOR)), G_CMAX) / C : 0.0;
  A *= k;
  B *= k;
  float warm = 0.0;
  if (k > 0.0 && hue >= WARM_FROM && hue <= WARM_TO) warm = smooth01(min((hue - WARM_FROM) / WARM_IN, (WARM_TO - hue) / WARM_OUT));
  if (warm > 0.0) {
    float c = sqrt(A * A + B * B);
    float genuine = smooth01((source - WARM_PALE) / (WARM_RICH - WARM_PALE));
    float kept = min(c, WARM_BREATH + (c - WARM_BREATH) * genuine);
    float f = c > EPS6 ? (c + warm * (kept - c)) / c : 1.0;
    A *= f;
    B *= f;
  }
  return capInto(Lo, A, B);
}
`,Do=`
uniform sampler2D uSumC;
uniform float uS3;
uniform float uYcap;

float kcOf() {
  vec4 s = texelFetch(uSumC, ivec2(0), 0);
  float meanC = s.x / max(1.0, s.y);
  return meanC > C_MEAN * uS3 ? pow(C_MEAN * uS3 / meanC, C_BUDGET_EXP) : 1.0;
}
`,Yt={box:`
uniform sampler2D uTex;
uniform ivec2 uDir;
uniform int uR;
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy), last = textureSize(uTex, 0) - 1;
  vec4 acc = vec4(0.0);
  for (int k = -uR; k <= uR; k++) acc += texelFetch(uTex, clamp(p + uDir * k, ivec2(0), last), 0);
  outColor = acc / float(2 * uR + 1);
}`,sum:`
uniform sampler2D uTex;
void main() {
  ivec2 o = ivec2(gl_FragCoord.xy) * BLOCK, size = textureSize(uTex, 0);
  vec4 s = vec4(0.0);
  for (int j = 0; j < BLOCK; j++) for (int i = 0; i < BLOCK; i++) {
    ivec2 q = o + ivec2(i, j);
    if (q.x < size.x && q.y < size.y) s += texelFetch(uTex, q, 0);
  }
  outColor = s;
}`,grade:`${No}
uniform sampler2D uSrc;
uniform ivec2 uOrigin;
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  vec3 lab = labOfLinear(linOf3(texelFetch(uSrc, uOrigin + p, 0).rgb));
  outColor = vec4(gradeInto(lab), lab.x);
}`,detail:`${No}
uniform sampler2D uGlab;
uniform sampler2D uBase;
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  vec4 g = texelFetch(uGlab, p, 0), b = texelFetch(uBase, p, 0);
  float Ln = max(DETAIL_FLOOR, b.x + LOCAL_GAIN * (g.w - b.w));
  float k = min(DETAIL_KMAX, max(DETAIL_KMIN, sqrt(Ln / max(g.x, DETAIL_FLOOR))));
  vec3 lab = capInto(Ln, g.y * k, g.z * k);
  outColor = vec4(clamp(linearOfLab(lab), 0.0, 1.0), length(lab.yz));
}`,layout:`
uniform sampler2D uGraded;
uniform sampler2D uCols;
uniform bool uFramed;
uniform vec4 uFrame;
uniform vec3 uClosed;
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  vec2 size = vec2(textureSize(uGraded, 0));
  float u, v;
  if (uFramed) {
    vec4 col = texelFetch(uCols, ivec2(p.x, 0), 0);
    u = mirror(col.x);
    v = mirror(0.5 + ((float(p.y) + 0.5 - uFrame.y) / uFrame.w - 0.5) / col.y);
  } else {
    u = (float(p.x) + 0.5 - uClosed.x) / (size.x * uClosed.z);
    v = (float(p.y) + 0.5 - uClosed.y) / (size.y * uClosed.z);
  }
  outColor = bilinear(uGraded, u, v);
}`,mix:`
uniform sampler2D uLensed;
uniform sampler2D uSofter;
uniform sampler2D uCols;
uniform bool uFramed;
uniform bool uCover;
uniform vec4 uFrame;
uniform float uCorner;
uniform vec2 uPool;
uniform vec2 uBar;
uniform float uH;
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  float x = float(p.x) + 0.5, y = float(p.y) + 0.5;
  vec4 l = texelFetch(uLensed, p, 0);
  if (uCover && uFrame.x - x <= 0.0) {
    float R = uCorner;
    float dx = uFrame.x + R - x, dy = max(R - y, y - (uH - R));
    float t = dx <= 0.0 || dy <= 0.0 ? 1.0 : clamp01(R - sqrt(dx * dx + dy * dy) + 0.5);
    if (t > 0.0) l += (texelFetch(uSofter, p, 0) - l) * t;
  }
  vec3 lab = labOfLinear(l.rgb);
  float c = length(lab.yz);
  float kr = c > EPS5 ? min(GIVE_MAX, max(1.0, l.a * GIVE_BACK / c)) : 1.0;
  float f, Fc;
  if (uCover) {
    float reach = 1.0 - smooth01((uFrame.x - x) / (POOL_REACH * max(1.0, uFrame.x)));
    float drop = POOL * pow(min(1.0, abs(y - uPool.x) / uPool.y), POOL_EXP);
    f = texelFetch(uCols, ivec2(p.x, 0), 0).z * (1.0 - drop * reach);
    Fc = pow(f, FALL_CHROMA);
  } else if (uFramed) {
    vec4 col = texelFetch(uCols, ivec2(p.x, 0), 0);
    f = col.z;
    Fc = col.w;
  } else {
    float r = length(vec2(x - uBar.x, y - uH)) / uBar.y;
    f = 1.0 - (r < BAR_KNEE ? r : BAR_KNEE + (r - BAR_KNEE) * BAR_TAIL);
    Fc = pow(max(f, 0.0), FALL_CHROMA);
  }
  outColor = vec4(lab.x * f, lab.yz * (kr * Fc), 0.0);
}`,measureC:`
uniform sampler2D uLab;
uniform int uMeasureTo;
void main() {
  ivec2 o = ivec2(gl_FragCoord.xy) * BLOCK, size = textureSize(uLab, 0);
  vec4 s = vec4(0.0);
  for (int j = 0; j < BLOCK; j++) for (int i = 0; i < BLOCK; i++) {
    ivec2 q = o + ivec2(i, j);
    if (q.x < min(size.x, uMeasureTo) && q.y < size.y) s += vec4(length(texelFetch(uLab, q, 0).yz), 1.0, 0.0, 0.0);
  }
  outColor = s;
}`,measureYP:`${Do}
uniform sampler2D uLab;
uniform int uMeasureTo;
void main() {
  float kc = kcOf();
  ivec2 o = ivec2(gl_FragCoord.xy) * BLOCK, size = textureSize(uLab, 0);
  vec4 s = vec4(0.0);
  for (int j = 0; j < BLOCK; j++) for (int i = 0; i < BLOCK; i++) {
    ivec2 q = o + ivec2(i, j);
    if (q.x >= min(size.x, uMeasureTo) || q.y >= size.y) continue;
    vec3 lab = texelFetch(uLab, q, 0).xyz;
    lab.yz *= kc;
    vec3 rgb = clamp(linearOfLab(lab), 0.0, 1.0);
    float y = lumOf(rgb), t = 1.0;
    if (y > uYcap) {
      t = cbrt(uYcap / y);
      y = uYcap;
    }
    s += vec4(y, perceived(lab) * t, 1.0, 0.0);
  }
  outColor = s;
}`,raw:`
uniform sampler2D uSrc;
uniform ivec2 uOrigin;
void main() {
  outColor = vec4(linOf3(texelFetch(uSrc, uOrigin + ivec2(gl_FragCoord.xy), 0).rgb), 1.0);
}`},Ho=`${Do}
uniform sampler2D uLab;
uniform sampler2D uSumYP;
uniform sampler2D uEdge;
uniform float uScale;
uniform float uPx;
uniform bool uSeam;
uniform ivec2 uSeamX;
uniform vec4 uFrame;
uniform float uRw;

float gainOf() {
  vec4 s = texelFetch(uSumYP, ivec2(0), 0);
  float n = max(1.0, s.z);
  float r = P_MEAN * uS3 / max(EPS6, s.y / n);
  return min(1.0, min(MEAN * uScale / max(EPS6, s.x / n), r * r * r));
}
vec3 hazeLinear(ivec2 p, float kc, float gain) {
  vec3 lab = texelFetch(uLab, p, 0).xyz;
  vec3 rgb = clamp(linearOfLab(vec3(lab.x, lab.yz * kc)), 0.0, 1.0);
  float y = lumOf(rgb);
  if (y > uYcap) rgb *= uYcap / y;
  return rgb * gain;
}
vec3 withSeam(vec3 rgb, ivec2 p) {
  if (!uSeam || p.x < uSeamX.x || p.x >= uSeamX.y) return rgb;
  float d = (uFrame.x - (float(p.x) + 0.5)) * uPx;
  float a = d > 0.0 ? 1.0 - smooth01(d / SEAM) : 1.0 - smooth01((-d - SEAM_HOLD * SEAM_IN) / SEAM_IN);
  if (a <= 0.0) return rgb;
  float v = mirror((float(p.y) + 0.5 - uFrame.y) / uFrame.w);
  float ew = float(textureSize(uEdge, 0).x);
  vec3 light = bilinear(uEdge, (abs(d) / uPx / uFrame.z * uRw) / ew, v).rgb * SEAM_DIM;
  return rgb + (light - rgb) * a;
}
`;Yt.haze=`${Ho}
uniform uint uKey;
uniform int uW;
uniform int uH;
void main() {
  ivec2 p = ivec2(int(gl_FragCoord.x), uH - 1 - int(gl_FragCoord.y));
  vec3 e = encOf3(withSeam(hazeLinear(p, kcOf(), gainOf()), p));
  int i = (p.y * uW + p.x) * 4;
  outColor = vec4(floor(e.r * 255.0 + noiseAt(i, uKey)), floor(e.g * 255.0 + noiseAt(i + 1, uKey)), floor(e.b * 255.0 + noiseAt(i + 2, uKey)), 255.0) / 255.0;
}`,Yt.half=`${Ho}
uniform bool uLit;
uniform int uW;
uniform int uH;
vec3 encodedAt(ivec2 p, float kc, float gain) {
  vec3 rgb = hazeLinear(p, kc, gain);
  return encOf3(uLit ? withSeam(rgb, p) : rgb);
}
void main() {
  float kc = kcOf(), gain = gainOf();
  ivec2 h = ivec2(gl_FragCoord.xy);
  int x0 = 2 * h.x, y0 = 2 * h.y, x1 = min(uW - 1, x0 + 1), y1 = min(uH - 1, y0 + 1);
  vec3 s = encodedAt(ivec2(x0, y0), kc, gain) + encodedAt(ivec2(x1, y0), kc, gain) + encodedAt(ivec2(x0, y1), kc, gain) + encodedAt(ivec2(x1, y1), kc, gain);
  outColor = vec4(s / 4.0, 1.0);
}`,Yt.frost=`
uniform sampler2D uFrost;
uniform sampler2D uFrostLit;
uniform bool uSpill;
uniform mat3 uSat;
uniform float uBright;
uniform vec4 uFrame;
uniform float uPx;
uniform uint uKey;
uniform int uW;
uniform int uH;
/* The gathered twin (haze.js, parseEdge): the frost under the plane's tint,
   through --frost-edge, less the tint laid back on it. */
uniform bool uGather;
uniform float uGain;
uniform mat3 uEdgeSat;
uniform float uAlpha;
uniform vec3 uTint;
vec3 frosted(vec3 b) { return clamp(clamp(uSat * b, 0.0, 1.0) * uBright, 0.0, 1.0); }
vec3 gathered(vec3 v) {
  vec3 f = clamp(uEdgeSat * clamp(mix(v, uTint, uAlpha) * uGain, 0.0, 1.0), 0.0, 1.0);
  return clamp((f - uAlpha * uTint) / (1.0 - uAlpha), 0.0, 1.0);
}
void main() {
  ivec2 p = ivec2(int(gl_FragCoord.x), uH - 1 - int(gl_FragCoord.y));
  vec3 v = frosted(texelFetch(uFrost, p, 0).rgb);
  if (uSpill) {
    float k = 1.0 - smooth01(((uFrame.x - 2.0 * (float(p.x) + 0.5)) * uPx - GAP) / SPILL);
    if (k > 0.0) v += (frosted(texelFetch(uFrostLit, p, 0).rgb) - v) * k;
  }
  if (uGather) v = gathered(v);
  int i = (p.y * uW + p.x) * 4;
  outColor = vec4(floor(v.r * 255.0 + noiseAt(i, uKey)), floor(v.g * 255.0 + noiseAt(i + 1, uKey)), floor(v.b * 255.0 + noiseAt(i + 2, uKey)), 255.0) / 255.0;
}`;function pa(){const t=`#version 300 es
precision highp float;
precision highp int;
precision highp sampler2D;
${ga()}
${Ea}`;return Object.fromEntries(Object.entries(Yt).map(([e,o])=>[e,t+o]))}function va(){if(typeof OffscreenCanvas!="function")return null;const t=new OffscreenCanvas(1,1),e=t.getContext("webgl2",{alpha:!1,antialias:!1,depth:!1,stencil:!1,preserveDrawingBuffer:!1});if(!e)return null;let o=null,a=null,r=!1;function i(){if(!e.getExtension("EXT_color_buffer_float"))throw new Error("no float render targets");e.disable(e.DITHER);const l=c(e.VERTEX_SHADER,_a);o={};for(const[h,y]of Object.entries(pa())){const _=c(e.FRAGMENT_SHADER,y),A=e.createProgram();if(e.attachShader(A,l),e.attachShader(A,_),e.linkProgram(A),e.deleteShader(_),!e.getProgramParameter(A,e.LINK_STATUS))throw new Error(`haze ${h}: ${e.getProgramInfoLog(A)}`);const m={};for(let f=0;f<e.getProgramParameter(A,e.ACTIVE_UNIFORMS);f++){const d=e.getActiveUniform(A,f);m[d.name]={loc:e.getUniformLocation(A,d.name),type:d.type}}o[h]={p:A,uniforms:m}}e.deleteShader(l),a=e.createFramebuffer()}function c(l,h){const y=e.createShader(l);if(e.shaderSource(y,h),e.compileShader(y),!e.getShaderParameter(y,e.COMPILE_STATUS))throw new Error(`haze shader: ${e.getShaderInfoLog(y)}`);return y}t.addEventListener("webglcontextlost",l=>{l.preventDefault(),r=!0}),t.addEventListener("webglcontextrestored",()=>{try{i(),r=!1}catch{}}),i();let s=[];function n(l,h,y=null,_=!1){const A=e.createTexture();return e.bindTexture(e.TEXTURE_2D,A),e.texParameteri(e.TEXTURE_2D,e.TEXTURE_MIN_FILTER,e.NEAREST),e.texParameteri(e.TEXTURE_2D,e.TEXTURE_MAG_FILTER,e.NEAREST),e.texParameteri(e.TEXTURE_2D,e.TEXTURE_WRAP_S,e.CLAMP_TO_EDGE),e.texParameteri(e.TEXTURE_2D,e.TEXTURE_WRAP_T,e.CLAMP_TO_EDGE),_?e.texImage2D(e.TEXTURE_2D,0,e.RGBA8,l,h,0,e.RGBA,e.UNSIGNED_BYTE,y):e.texImage2D(e.TEXTURE_2D,0,e.RGBA32F,l,h,0,e.RGBA,e.FLOAT,y),s.push(A),{t:A,w:l,h}}function u(l,h,y){const{p:_,uniforms:A}=o[l];e.useProgram(_);let m=0;for(const[f,d]of Object.entries(y)){const E=A[f];if(E)switch(E.type){case e.SAMPLER_2D:e.activeTexture(e.TEXTURE0+m),e.bindTexture(e.TEXTURE_2D,d.t),e.uniform1i(E.loc,m++);break;case e.FLOAT:e.uniform1f(E.loc,d);break;case e.FLOAT_VEC2:e.uniform2fv(E.loc,d);break;case e.FLOAT_VEC3:e.uniform3fv(E.loc,d);break;case e.FLOAT_VEC4:e.uniform4fv(E.loc,d);break;case e.INT:e.uniform1i(E.loc,d);break;case e.BOOL:e.uniform1i(E.loc,d?1:0);break;case e.INT_VEC2:e.uniform2iv(E.loc,d);break;case e.UNSIGNED_INT:e.uniform1ui(E.loc,d);break;case e.FLOAT_MAT3:e.uniformMatrix3fv(E.loc,!1,d);break}}h?(e.bindFramebuffer(e.FRAMEBUFFER,a),e.framebufferTexture2D(e.FRAMEBUFFER,e.COLOR_ATTACHMENT0,e.TEXTURE_2D,h.t,0),e.viewport(0,0,h.w,h.h)):(e.bindFramebuffer(e.FRAMEBUFFER,null),e.viewport(0,0,t.width,t.height)),e.drawArrays(e.TRIANGLES,0,3)}function x(l,h){const y=Mo(h),_=n(l.w,l.h),A=n(l.w,l.h);let m=l;for(let f=0;f<3;f++)u("box",_,{uTex:m,uDir:[1,0],uR:y}),u("box",A,{uTex:_,uDir:[0,1],uR:y}),m=A;return A}function g(l,h,y,_){let A=n(Math.ceil(h/St),Math.ceil(y/St));for(u(l,A,_);A.w>1||A.h>1;){const m=n(Math.ceil(A.w/St),Math.ceil(A.h/St));u("sum",m,{uTex:A}),A=m}return A}function M(l,h,y,{scale:_=1,seed:A=1,frost:m=null}={}){if(r||e.isContextLost())return null;const f=Po(l,h,y,_),{w:d,h:E,rw:C,rh:S,frame:w,ex:U}=f,[T,j]=wo(A);try{t.width=d,t.height=E;const R=n(l.w,l.h,new Uint8Array(l.data.buffer,l.data.byteOffset,l.data.length),!0),et=[f.rect[0],f.rect[1]],G={uEx:[U.lo,U.mid,U.hi,U.M],uTint:f.tintLab,uS3:f.s3,uYcap:f.ycap},X=n(C,S);u("grade",X,{...G,uSrc:R,uOrigin:et});const z=n(C,S);u("detail",z,{...G,uGlab:X,uBase:x(X,f.local)});const K=w?[w.x,w.y,w.w,w.h]:[0,0,1,1],Q=f.cols?n(d,1,f.cols):n(1,1,new Float32Array(4)),B=f.closed?[f.closed.ox,f.closed.oy,f.closed.k]:[0,0,1],Z=n(d,E);u("layout",Z,{uGraded:z,uCols:Q,uFramed:!!w,uFrame:K,uClosed:B});const ot=x(Z,f.lens),J=n(d,E);u("mix",J,{uLensed:ot,uSofter:f.cover?x(Z,f.softer):ot,uCols:Q,uFramed:!!w,uCover:f.cover,uFrame:K,uCorner:f.corner,uPool:f.pool?[f.pool.cy,f.pool.span]:[0,1],uBar:f.closed?[f.closed.cx,f.closed.radius]:[0,1],uH:E});const Vt={uSumC:g("measureC",d,E,{uLab:J,uMeasureTo:f.measureTo}),uS3:f.s3,uYcap:f.ycap},ge=g("measureYP",d,E,{...Vt,uLab:J,uMeasureTo:f.measureTo});let v=ot;if(f.seam){const O=n(f.seam.ew,S);u("raw",O,{uSrc:R,uOrigin:et}),v=x(O,f.seam.soft)}const P={...Vt,uLab:J,uSumYP:ge,uEdge:v,uScale:f.scale,uPx:f.px,uSeam:!!f.seam,uSeamX:f.seam?[f.seam.x0,f.seam.x1]:[0,0],uFrame:K,uRw:C,uW:d,uH:E};u("haze",null,{...P,uKey:T});let p;if(w&&!f.immersive){const O=Math.min(d-1,Math.ceil(w.x)),b=d-O,N=new Uint8Array(b*E*4);e.readPixels(O,0,b,E,e.RGBA,e.UNSIGNED_BYTE,N),p=xt({data:N,w:b,h:E},[0,0,b,E],!0)}else p=xt(l,f.shown);const k=t.transferToImageBitmap();let q=null,Y=null,$=null;if(m){const O=Math.ceil(d/2),b=Math.ceil(E/2),N=m.sigma/2,ct=n(O,b);u("half",ct,{...P,uLit:!1});const D=x(ct,N);let st=D;if(f.seam){const ht=n(O,b);u("half",ht,{...P,uLit:!0}),st=x(ht,N)}t.width=O,t.height=b;const W=Xt(m.saturate),L=m.edge,tt=Xt((L==null?void 0:L.saturate)??1),Qt={uFrost:D,uFrostLit:st,uSpill:!!f.seam,uSat:[W[0],W[3],W[6],W[1],W[4],W[7],W[2],W[5],W[8]],uBright:m.brightness,uFrame:K,uPx:f.px,uKey:j,uW:O,uH:b,uGather:!1,uGain:(L==null?void 0:L.gain)??1,uEdgeSat:[tt[0],tt[3],tt[6],tt[1],tt[4],tt[7],tt[2],tt[5],tt[8]],uAlpha:(L==null?void 0:L.alpha)??0,uTint:(L==null?void 0:L.tint)??[0,0,0]};u("frost",null,Qt);const Wo=new Uint8Array(Ht.length*b*4);if(Ht.forEach((ht,Ct)=>{e.readPixels(Math.min(O-1,Math.floor(O*ht)),0,1,b,e.RGBA,e.UNSIGNED_BYTE,Wo.subarray(Ct*b*4,(Ct+1)*b*4))}),$=mo(Wo),q=t.transferToImageBitmap(),L){u("frost",null,{...Qt,uGather:!0});const ht=new Uint8Array(O*b*4);e.readPixels(0,0,O,b,e.RGBA,e.UNSIGNED_BYTE,ht);const Ct=new Uint8ClampedArray(O*b*4);for(let Tt=0;Tt<b;Tt++)Ct.set(ht.subarray((b-1-Tt)*O*4,(b-Tt)*O*4),Tt*O*4);Y={data:Ct,w:O,h:b}}}return e.isContextLost()?(k.close(),q==null||q.close(),null):{bitmap:k,frost:q,gather:Y,tone:$,grey:f.grey,light:p}}finally{for(const R of s)e.deleteTexture(R);s=[],t.width=t.height=1}}return{render:M,context:e}}const Ae=new OffscreenCanvas(lt,lt),jt=Ae.getContext("2d");jt.imageSmoothingQuality="high";const $t=new OffscreenCanvas(lt,lt).getContext("2d",{willReadFrequently:!0}),gt=new OffscreenCanvas(1,1),ba=gt.getContext("2d");let _t=null,F=null,I=null,Oa=1;function Uo(t){const e=t.displayWidth||t.width,o=t.displayHeight||t.height;if(!e||!o)return;const a=lt/Math.max(e,o),r=Math.max(8,Math.round(e*a)),i=Math.max(8,Math.round(o*a));jt.clearRect(0,0,r,i),jt.drawImage(t,0,0,r,i);try{return $t.clearRect(0,0,r,i),$t.drawImage(Ae,0,0,r,i,0,0,r,i),{data:$t.getImageData(0,0,r,i).data,w:r,h:i}}catch{return Ae.width=lt,$t.canvas.width=lt,jt.imageSmoothingQuality="high",null}}const zo=(t,e)=>({top:Math.max(t.top,e.top),bottom:Math.max(t.bottom,e.bottom)}),La={top:1,bottom:1};function Sa(t,e,o){const a=fa(t,e.geo,e.tint,o),r=(i,c,s)=>((gt.width!==c||gt.height!==s)&&(gt.width=c,gt.height=s),ba.putImageData(new ImageData(i,c,s),0,0),gt.transferToImageBitmap());return{bitmap:r(a.rgba,e.geo.w,e.geo.h),frost:a.frost?r(a.frost,a.frostW,a.frostH):null,gather:a.gather?{data:a.gather,w:a.frostW,h:a.frostH}:null,tone:a.tone,grey:a.grey,light:a.light}}function Ca(t,e,o){if(!_t)return null;try{return _t.render(t,e.geo,e.tint,o)}catch(a){return console.warn("haze: the GPU renderer failed; drawing on the CPU from now on.",a),_t=!1,null}}function Ko(t,e,o=null,a=void 0){var x;const r=performance.now(),i=e??Aa(t.tint),c={scale:t.scale,seed:Oa++,frost:t.frost??null},s=Ca(i,t,c),n=s??Sa(i,t,c),u=e?o?zo(o,n.light):n.light:La;postMessage({type:"haze",seq:t.seq,key:t.key,fade:t.fade,bitmap:n.bitmap,frost:n.frost,gather:n.gather,tone:n.tone,grey:n.grey,light:u,distance:a,gpu:!!s,ms:performance.now()-r},[n.bitmap,n.frost,(x=n.gather)==null?void 0:x.data.buffer].filter(Boolean))}onmessage=({data:t})=>{var e;switch(t.type){case"init":{if(t.cpu){_t=!1;break}try{_t=va()??!1}catch(o){console.warn("haze: no GPU renderer; drawing on the CPU.",o),_t=!1}break}case"render":{if(t.source!==void 0){const o=t.source?Uo(t.source):null;if((e=t.source)==null||e.close(),o===void 0)break;if(o&&t.video&&!t.allowBlank&&Dn(o)){postMessage({type:"blank",seq:t.seq});break}(F==null?void 0:F.key)!==t.key&&(I=null),F={key:t.key,src:o}}if((F==null?void 0:F.key)!==t.key)break;Ko(t,F.src,(I==null?void 0:I.key)===t.key?I.light:null);break}case"sample":{const o=(F==null?void 0:F.key)===t.key&&F.src?Uo(t.frame):null;if(t.frame.close(),!o)break;const a=xt(o,Dt(o,t.geo).shown);(I==null?void 0:I.key)!==t.key&&(I={key:t.key,frames:[F.src],light:xt(F.src,Dt(F.src,t.geo).shown)});const r=I.light;I.frames.push(o),I.light=zo(r,a),(I.light.top>r.top+.02||I.light.bottom>r.bottom+.02)&&postMessage({type:"light",key:t.key,light:I.light});break}case"settle":{if(!I||I.key!==t.key||(F==null?void 0:F.key)!==t.key){postMessage({type:"done",seq:t.seq,key:t.key,light:null});break}const{frames:o,light:a}=I;I=null;const r=o.map(s=>Xn(s,Dt(s,t.geo).rect)),i=Yn(r),c=i===0?0:xo(r[i],r[0]);c>jn?(F={key:t.key,src:o[i]},Ko(t,F.src,a,c)):postMessage({type:"done",seq:t.seq,key:t.key,light:a,distance:c});break}}}})();
