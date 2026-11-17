(function(){"use strict";const Yt={peak:.1,mean:.045},it=360,Me=.04045,jt=12.92,_t=.055,$t=2.4,ye=.0031308,Ct=.2126,Rt=.7152,St=.0722,Vt=.001,Qt=1e-4,de=1e-5,ct=1e-6,Ae=t=>t<=Me?t/jt:((t+_t)/(1+_t))**$t,Uo=t=>t<=ye?t*jt:(1+_t)*t**(1/$t)-_t,V=t=>Math.min(1,Math.max(0,t)),_e=new Float32Array(4097);for(let t=0;t<=4096;t++)_e[t]=Ae(t/4096);const Zt=t=>_e[t>=1?4096:t<=0?0:t*4096+.5|0],H=new Float32Array(256);for(let t=0;t<256;t++)H[t]=Ae(t/255);const Tt=new Float32Array(16385);for(let t=0;t<=16384;t++)Tt[t]=Uo(t/16384);function ft(t){if(t<=0)return 0;if(t>=1)return 1;const e=t*16384,o=e|0;return Tt[o]+(Tt[o+1]-Tt[o])*(e-o)}const ge=(t,e)=>Ct*H[t[e]]+Rt*H[t[e+1]]+St*H[t[e+2]],Ft=new Float64Array(8193);for(let t=0;t<=8192;t++)Ft[t]=(t/8192)**(4/3);function wt(t){if(!(t>=0&&t<=1))return Math.cbrt(t);const e=Math.sqrt(Math.sqrt(t))*8192,o=e|0;return o>=8192?1:Ft[o]+(Ft[o+1]-Ft[o])*(e-o)}const It=new Float64Array(4097);for(let t=0;t<=4096;t++)It[t]=(t/4096)**.6;function Ee(t){if(t<=0)return 0;if(t>=1)return 1;const e=t*4096,o=e|0;return It[o]+(It[o+1]-It[o])*(e-o)}const pe=[.4122214708,.5363325363,.0514459929,.2119034982,.6806995451,.1073969566,.0883024619,.2817188376,.6299787005],ve=[.2104542553,.793617785,-.0040720468,1.9779984951,-2.428592205,.4505937099,.0259040371,.7827717662,-.808675766],be=[1,.3963377774,.2158037573,1,-.1055613458,-.0638541728,1,-.0894841775,-1.291485548],Oe=[4.0767416621,-3.3077115913,.2309699292,-1.2684380046,2.6097574011,-.3413193965,-.0041960863,-.7034186147,1.707614701],[zo,Ko,qo,Wo,Xo,Yo,jo,$o,Vo]=pe,[Qo,Zo,Jo,tn,en,on,nn,an,rn]=ve,[,cn,sn,,ln,un,,fn,mn]=be,[hn,xn,Mn,yn,dn,An,_n,gn,En]=Oe;function gt(t,e,o,a){const r=wt(zo*t+Ko*e+qo*o),i=wt(Wo*t+Xo*e+Yo*o),c=wt(jo*t+$o*e+Vo*o);return a[0]=Qo*r+Zo*i+Jo*c,a[1]=tn*r+en*i+on*c,a[2]=nn*r+an*i+rn*c,a}function Jt(t,e,o,a){const r=t+cn*e+sn*o,i=t+ln*e+un*o,c=t+fn*e+mn*o,s=r*r*r,n=i*i*i,u=c*c*c;return a[0]=hn*s+xn*n+Mn*u,a[1]=yn*s+dn*n+An*u,a[2]=_n*s+gn*n+En*u,a}function pn(t,e,o){return gt(Zt(t),Zt(e),Zt(o),[0,0,0])}const ot=38,lt=156,Pt=54,te=138,vn=(ot+lt)/2,Le=t=>t-360*Math.floor(t/360);function bn(t,e=vn){const o=Le(t);return o<ot||o>lt?o:o<e?ot+(o-ot)/(e-ot)*(Pt-ot):te+(o-e)/(lt-e)*(lt-te)}function On(t){return t>Pt?0:Math.max(0,(t-ot)/(Pt-ot))}const ee=.04,Ce=.5,Re=.3,Se=.12,Te=.22,Fe=.04,oe=.05,we=.13,ne=.018,Et=22,Ie=14,ae=64,Pe=6;function Ln(t){const e=Le(t);return e<Et||e>ae?0:nt(Math.min((e-Et)/Ie,(ae-e)/Pe))}const ke=[Math.cos(Et*Math.PI/180),Math.sin(Et*Math.PI/180)],Be=[Math.cos(lt*Math.PI/180),Math.sin(lt*Math.PI/180)],Ge=125,Cn=.97,kt=[0,0,0],re=.13,Rn=.3,De=.58,Ne=1.3,He=1.5,Ue=.2,ze=.05,Ke=.7,qe=.12,ce=.07,We=.9,Xe=.62,Ye=.33,je=.6,ut=1e-4,se=.88,$e=.995,Ve=16,Sn=.05,Qe=.8,ie=.03,Ze=.6,Je=1.4,to=.2,eo=.2,oo=4.8,no=Math.cos(oo),ao=Math.sin(oo);function ro(t,e,o){return t+to*Math.sqrt(e*e+o*o)+eo*Math.max(0,e*no+o*ao)}function Tn(t,e,o){return{lo:Math.min(t,e-.1),mid:e,hi:Math.max(o+.02,e+.15),M:Rn-.05*nt((.3-e)/.25)}}function Fn(t,e){return t<=e.mid?re+(e.M-re)*V((t-e.lo)/Math.max(Vt,e.mid-e.lo)):e.M+(De-e.M)*V((t-e.mid)/Math.max(Vt,e.hi-e.mid))**Ne}function wn(t,e,o,a,r,i,c,s){let n=Math.sqrt(e*e+o*o);const u=n,x=ee*Math.min(1,Math.max(Re,t/Ce)),g=V(1-n/x);if(g>0){const h=Math.sqrt(r[1]*r[1]+r[2]*r[2]);if(h>Qt){const f=Math.min(h,Se);e+=g*(r[1]/h*f-e),o+=g*(r[2]/h*f-o),n=Math.sqrt(e*e+o*o)}}let M=0,l=-1;if(ke[0]*o-ke[1]*e>=0&&e*Be[1]-o*Be[0]>=0&&(l=Math.atan2(o,e)*180/Math.PI,n>Qt&&l>=ot)){l=bn(l,Ge),M=On(l);const h=n*(1-Te*M);e=h*Math.cos(l*Math.PI/180),o=h*Math.sin(l*Math.PI/180),n=h}const m=(Fn(t,a)+Fe*M)*i,y=1+Ke*(1-nt(u/qe)),_=n>ct?Math.min(n*He*y*Math.sqrt(m/Math.max(t,ze)),Ue)/n:0;e*=_,o*=_;const A=_>0&&l>=0?Ln(l):0;if(A>0){const h=Math.sqrt(e*e+o*o),f=nt((u-oe)/(we-oe)),d=Math.min(h,ne+(h-ne)*f),E=h>ct?(h+A*(d-h))/h:1;e*=E,o*=E}return co(m,e,o,i,c,s)}function co(t,e,o,a,r,i){const c=Xe*a,s=ro(t,e,o);if(s>c){const n=Math.sqrt(e*e+o*o),u=(s-t)/Math.max(n,ct),x=Math.max(n*je,Math.min(n,(c-t)/u));e*=x/Math.max(n,ct),o*=x/Math.max(n,ct),t=Math.min(t,c-u*x)}for(let n=0;n<Ve;n++){Jt(t,e,o,kt);const u=kt[0],x=kt[1],g=kt[2];if(u<-ut||x<-ut||g<-ut||u>1+ut||x>1+ut||g>1+ut){e*=se,o*=se;continue}const M=Ct*u+Rt*x+St*g;if(M>r){t*=Math.cbrt(r/M)*$e;continue}break}return i[0]=t,i[1]=e,i[2]=o,i}function In(t,e,o,a=[0,0,e,o]){const i=(m,y)=>{const _=(y*e+m)*4;return t[_]<20&&t[_+1]<20&&t[_+2]<20};let[c,s,n,u]=a;const x=n-c,g=u-s,M=m=>{for(let y=c;y<n;y++)if(!i(y,m))return!1;return!0},l=m=>{for(let y=s;y<u;y++)if(!i(m,y))return!1;return!0};for(;s<a[1]+g*.4&&M(s);)s++;for(;u>a[3]-g*.4&&M(u-1);)u--;for(;c<a[0]+x*.4&&l(c);)c++;for(;n>a[2]-x*.4&&l(n-1);)n--;return[c,s,n,u]}function Bt(t,e){const{w:o,h:a}=t,r=Ro(e),i=r?r.w/r.h:null;let c=[0,0,o,a];if(i)if(o/a>i){const s=Math.max(1,Math.round(a*i)),n=Math.floor((o-s)/2);c=[n,0,n+s,a]}else{const s=Math.max(1,Math.round(o/i)),n=Math.floor((a-s)/2);c=[0,n,o,n+s]}return{shown:c,rect:In(t.data,o,a,c)}}function Pn(t){const e=t.data;for(let o=0;o<e.length;o+=4)if(ge(e,o)>.012)return!1;return!0}const so=2,pt=4096;function kn(t,e){const[o,a,r,i]=e,c=t.data,s=new Uint32Array(pt),n=[0,0,0],u=(ee*.5)**2;let x=0,g=0;for(let l=a;l<i;l+=so)for(let m=o;m<r;m+=so){const y=(l*t.w+m)*4;gt(H[c[y]],H[c[y+1]],H[c[y+2]],n),s[Math.min(pt-1,Math.max(0,n[0]*pt|0))]++,n[1]*n[1]+n[2]*n[2]<u&&g++,x++}const M=l=>{const m=Math.min(x-1,Math.floor(l*x));let y=0;for(let _=0;_<pt;_++){if(y+s[_]>m)return(_+(m-y+.5)/s[_])/pt;y+=s[_]}return 1};return{ex:Tn(M(.05),M(.5),M(.97)),grey:g/Math.max(1,x)}}const Bn=.18,Gn=.58,Gt=[.07,.17];function io(t){let e=0,o=0,a=0;for(let i=0;i<t.length;i+=4)e+=t[i],o+=t[i+1],a+=t[i+2];const r=Math.max(1,t.length/4);return[Math.round(e/r),Math.round(o/r),Math.round(a/r)]}function Dn(t,e,o){const a=new Uint8Array(Gt.length*o*4);let r=0;for(const i of Gt){const c=Math.min(e-1,Math.floor(e*i));for(let s=0;s<o;s++){const n=(s*e+c)*4;a[r++]=t[n],a[r++]=t[n+1],a[r++]=t[n+2],a[r++]=t[n+3]}}return a}function mt(t,e,o=!1){const[a,r,i,c]=e,s=c-r,n=r+Math.max(1,Math.round(s*Bn)),u=r+Math.round(s*Gn),x=(g,M)=>{const l=new Float32Array((M-g)*(i-a));let m=0;for(let y=g;y<M;y++){const _=(o?t.h-1-y:y)*t.w;for(let A=a;A<i;A++)l[m++]=ge(t.data,(_+A)*4)}return l.length?Hn(Nn(l,Math.min(l.length-1,Math.floor(l.length*.9)))):0};return{top:x(r,n),bottom:x(u,c)}}function Nn(t,e){let o=0,a=t.length-1;for(;o<a;){const r=t[o+a>>1];let i=o,c=a;for(;i<=c;){for(;t[i]<r;)i++;for(;t[c]>r;)c--;if(i<=c){const s=t[i];t[i]=t[c],t[c]=s,i++,c--}}if(e<=c)a=c;else if(e>=i)o=i;else break}return t[e]}const Hn=t=>Math.round(t*1e3)/1e3,at={x:3,y:5};function Un(t,e){const[o,a,r,i]=e,c=new Float32Array(at.x*at.y*3),s=[0,0,0];for(let n=0;n<at.y;n++)for(let u=0;u<at.x;u++){const x=Math.floor(o+(r-o)*u/at.x),g=Math.max(x+1,Math.floor(o+(r-o)*(u+1)/at.x)),M=Math.floor(a+(i-a)*n/at.y),l=Math.max(M+1,Math.floor(a+(i-a)*(n+1)/at.y));let m=0,y=0,_=0,A=0;for(let h=M;h<l;h++)for(let f=x;f<g;f++){const d=(h*t.w+f)*4;m+=H[t.data[d]],y+=H[t.data[d+1]],_+=H[t.data[d+2]],A++}gt(m/A,y/A,_/A,s),c.set(s,(n*at.x+u)*3)}return c}function lo(t,e){let o=0;for(let a=0;a<t.length;a+=3)o+=Math.hypot(t[a]-e[a],t[a+1]-e[a+1],t[a+2]-e[a+2]);return o/(t.length/3)}function zn(t){let e=0,o=1/0;for(let a=0;a<t.length;a++){let r=0;for(let i=0;i<t.length;i++)a!==i&&(r+=lo(t[a],t[i]));r<o&&(o=r,e=a)}return e}const Kn=.08,uo=t=>Math.max(1,Math.round(t-.5));function fo(t,e,o,a,r,i){const c=1/(2*r+1),s=o*i;for(let n=0;n<a;n++){const u=n*s;for(let x=0;x<i;x++){const g=u+x,M=g+(o-1)*i;let l=0;for(let m=-r;m<=r;m++)l+=t[g+(m<0?0:m>o-1?o-1:m)*i];for(let m=g,y=g+(r+1)*i,_=g-r*i;m<=M;m+=i,y+=i,_+=i)e[m]=l*c,l+=t[y<M?y:M]-t[_>g?_:g]}}}function mo(t,e,o,a,r,i,c){const s=1/(2*r+1),n=o*i;c.fill(0,0,n);for(let u=-r;u<=r;u++){const x=(u<0?0:u>a-1?a-1:u)*n;for(let g=0;g<n;g++)c[g]+=t[x+g]}for(let u=0;u<a;u++){const x=u*n,g=(u+r+1<a?u+r+1:a-1)*n,M=(u-r>0?u-r:0)*n;for(let l=0;l<n;l++)e[x+l]=c[l]*s,c[l]+=t[g+l]-t[M+l]}}function vt(t,e,o,a,r=3){const i=uo(a),c=new Float32Array(t.length),s=new Float32Array(t.length),n=new Float64Array(e*r);fo(t,s,e,o,i,r),mo(s,c,e,o,i,r,n);for(let u=1;u<3;u++)fo(c,s,e,o,i,r),mo(s,c,e,o,i,r,n);return c}const qn=6,ho=3,Wn=18,xo=2.4,Xn=.55,Yn=.2,jn=1.2,Mo=.6,yo=.2,Ao=1.3,$n=.44,_o=.6,go=.85,Eo=2.2,Vn=.06,Qn=1.05,Dt=.5,po=.72,Nt=44,bt=22,vo=.5,bo=1.5,Ht=.9,Zn=10,le=8,ue=16,ht=.213,xt=.715,Mt=.072,Oo=2146121005,Lo=2221713035,Ut=24;function fe(t){return t^=t>>>16,t=Math.imul(t,Oo),t^=t>>>15,t=Math.imul(t,Lo),t^=t>>>16,t>>>0}const Co=t=>[fe(2*t),fe(2*t+1)],yt=(t,e)=>(fe((t^e)>>>0)>>>32-Ut)/2**Ut,zt={floats:{SRGB_CUT:Me,SRGB_SLOPE:jt,SRGB_OFFSET:_t,SRGB_GAMMA:$t,SRGB_LINEAR_CUT:ye,LUMA_R:Ct,LUMA_G:Rt,LUMA_B:St,EPS3:Vt,EPS4:Qt,EPS5:de,EPS6:ct,ARC_LO:ot,ARC_HI:lt,WARM_TIP:Pt,GREEN_TOE:te,HAZE_FOLD:Ge,C_GREY:ee,GREY_L:Ce,GREY_FLOOR:Re,TINT_CMAX:Se,CLAY_SOFTEN:Te,CLAY_LIFT:Fe,WARM_PALE:oe,WARM_RICH:we,WARM_BREATH:ne,WARM_FROM:Et,WARM_IN:Ie,WARM_TO:ae,WARM_OUT:Pe,G_SHADOW:re,G_LIGHT:De,G_HI_GAMMA:Ne,G_CHROMA:He,G_CMAX:Ue,L_FLOOR:ze,G_VIBRANCE:Ke,VIB_KNEE:qe,C_MEAN:ce,C_BUDGET_EXP:We,P_PEAK:Xe,P_MEAN:Ye,CAP_KEEP:je,GAMUT_TOL:ut,GAMUT_STEP:se,Y_MARGIN:$e,LOCAL_GAIN:Qe,DETAIL_FLOOR:ie,DETAIL_KMIN:Ze,DETAIL_KMAX:Je,HK_C:to,HK_BLUE:eo,HK_A:no,HK_B:ao,FALL_CHROMA:Mo,POOL:yo,POOL_EXP:Ao,POOL_REACH:_o,GIVE_BACK:go,GIVE_MAX:Eo,BAR_KNEE:Dt,BAR_TAIL:po,SEAM:Nt,SEAM_IN:bt,SEAM_HOLD:vo,SEAM_DIM:Ht,GAP:le,SPILL:ue,MEAN:Yt.mean,NOISE_SCALE:2**Ut},ints:{CAP_ITER:Ve,NOISE_SHIFT:32-Ut},uints:{HASH_M1:Oo,HASH_M2:Lo},mat3:{LMS_OF_RGB:pe,LAB_OF_LMS:ve,LMS_OF_LAB:be,RGB_OF_LMS:Oe}},nt=t=>t<=0?0:t>=1?1:t*t*(3-2*t),me=t=>{const e=Math.abs(t),o=e-2*Math.floor(e/2);return o<=1?o:2-o};function Jn(t,e,o,a,r,i,c,s){const n=Math.min(e-1,Math.max(0,r*e-.5)),u=Math.min(o-1,Math.max(0,i*o-.5)),x=Math.max(0,Math.min(e-2,Math.floor(n))),g=Math.max(0,Math.min(o-2,Math.floor(u))),M=Math.min(e-1,x+1),l=Math.min(o-1,g+1),m=Math.min(1,Math.max(0,n-x)),y=Math.min(1,Math.max(0,u-g)),_=(g*e+x)*a,A=(g*e+M)*a,h=(l*e+x)*a,f=(l*e+M)*a;for(let d=0;d<a;d++){const E=t[_+d]+(t[A+d]-t[_+d])*m,C=t[h+d]+(t[f+d]-t[h+d])*m;c[s+d]=E+(C-E)*y}}function ta(t,e,o,a,r,i=e){const c=i-a,s=new Float32Array(c*o*r);for(let n=0;n<o;n++)s.set(t.subarray((n*e+a)*r,(n*e+i)*r),n*c*r);return s}function Ro(t){if(t.left==null)return null;if(t.video)return t.video;const e=t.left*t.w;return{x:e,y:0,w:t.w-e,h:t.h}}function ea(t,e){const o=new Float32Array(e*4),a=Xn*Math.max(1,t.x);let r=0,i=0;for(let c=e-1;c>=0;c--){const s=t.x-(c+.5),n=c*4;if(s<=0){o[n]=-s/t.w,o[n+1]=o[n+2]=o[n+3]=1;continue}r+=(s-i)/(t.w*(1+(xo-1)*nt((s+i)/2/a))),i=s,o[n]=r,o[n+1]=1+(xo-1)*nt(s/a),o[n+2]=1-Yn*(s/Math.max(1,t.x))**jn,o[n+3]=o[n+2]**Mo}return o}function So(t,e,o,a=1){const{w:r,h:i}=e,c=e.px??5,s=Ro(e),n=!!e.immersive,u=s&&e.video&&n?s:null,x=!!s&&!n,{shown:g,rect:M}=Bt(t,e),l=M[2]-M[0],m=M[3]-M[1],{ex:y,grey:_}=kn(t,M),A=qn/c,h=Math.max(r/l,i/m);let f=null;if(u){const d=Zn/(s.w*c/l);f={x0:Math.max(0,Math.floor(s.x-Nt/c)),x1:Math.min(r,Math.ceil(s.x+bo*bt/c)),soft:d,ew:Math.min(l,Math.ceil((Nt+bo*bt)/c/s.w*l+3*d)+2)}}return{w:r,h:i,px:c,frame:s,video:u,cover:x,immersive:n,shown:g,rect:M,rw:l,rh:m,ex:y,grey:_,seam:f,tintLab:pn(...o),scale:a,s3:Math.cbrt(a),ycap:Yt.peak*a*Cn,local:Sn*Math.max(l,m),lens:A,softer:A*ho,softX0:x?Math.max(0,Math.floor(s.x-3*A*ho)):0,corner:Wn/c,measureTo:s&&n?Math.max(1,Math.floor(s.x)):r,cols:s?ea(s,r):null,pool:x?oa(s):null,closed:s?null:{k:h,ox:(r-l*h)/2,oy:(i-m*h)/2,cx:r*Vn,radius:r*Qn}}}function oa(t){const e=t.y+t.h*$n;return{cy:e,span:Math.max(1,e-t.y,t.y+t.h-e)}}function na(t,e){return 1-nt((t.x-(e+.5))/(_o*Math.max(1,t.x)))}function aa(t,e){return Math.min(1,Math.abs(e+.5-t.cy)/t.span)**Ao}function ra(t,e,o,a){const r=e+.5-t.cx,i=o+.5-a,c=Math.sqrt(r*r+i*i)/t.radius;return 1-(c<Dt?c:Dt+(c-Dt)*po)}function ca(t,e,o,{scale:a=1,seed:r=1,frost:i=null}={}){const c=So(t,e,o,a),{w:s,h:n,frame:u,cols:x,closed:g,s3:M,ycap:l}=c,m=[0,0,0],y=[0,0,0],_=sa(t,c),{rw:A,rh:h}=c,f=new Float32Array(s*n*4),d=new Int32Array(s),E=new Int32Array(s),C=new Float32Array(s);for(let v=0;v<s;v++){const w=u?me(x[v*4]):(v+.5-g.ox)/(A*g.k),p=Math.min(A-1,Math.max(0,w*A-.5));d[v]=Math.max(0,Math.min(A-2,Math.floor(p))),E[v]=Math.min(A-1,d[v]+1),C[v]=Math.min(1,Math.max(0,p-d[v]))}for(let v=0;v<n;v++){const w=u?0:(v+.5-g.oy)/(h*g.k);for(let p=0;p<s;p++){const I=u?me(.5+((v+.5-u.y)/u.h-.5)/x[p*4+1]):w,W=Math.min(h-1,Math.max(0,I*h-.5)),Y=Math.max(0,Math.min(h-2,Math.floor(W))),O=Math.min(h-1,Y+1),L=Math.min(1,Math.max(0,W-Y)),B=C[p],j=(Y*A+d[p])*4,rt=(Y*A+E[p])*4,N=(O*A+d[p])*4,z=(O*A+E[p])*4,st=(v*s+p)*4;for(let P=0;P<4;P++){const Lt=_[j+P]+(_[rt+P]-_[j+P])*B,va=_[N+P]+(_[z+P]-_[N+P])*B;f[st+P]=Lt+(va-Lt)*L}}}const b=vt(f,s,n,c.lens,4),k=c.softX0,U=s-k,R=c.cover?vt(ta(f,s,n,k,4),U,n,c.softer,4):null,$=c.corner,S=(v,w)=>{if(u.x-(v+.5)>0)return 0;const p=u.x+$-(v+.5),I=Math.max($-(w+.5),w+.5-(n-$));return p<=0||I<=0?1:V($-Math.sqrt(p*p+I*I)+.5)},tt=c.measureTo,G=c.pool?Float32Array.from({length:s},(v,w)=>na(u,w)):null;let X=0,K=0;for(let v=0;v<n;v++){const w=c.pool?yo*aa(c.pool,v):0;for(let p=0;p<s;p++){const I=(v*s+p)*4;let W=b[I],Y=b[I+1],O=b[I+2],L=b[I+3];const B=R?S(p,v):0;if(B>0){const P=(v*U+p-k)*4;W+=(R[P]-W)*B,Y+=(R[P+1]-Y)*B,O+=(R[P+2]-O)*B,L+=(R[P+3]-L)*B}gt(W,Y,O,m);const j=Math.sqrt(m[1]*m[1]+m[2]*m[2]),rt=j>de?Math.min(Eo,Math.max(1,L*go/j)):1;let N,z;G?(N=x[p*4+2]*(1-w*G[p]),z=Ee(N)):u?(N=x[p*4+2],z=x[p*4+3]):(N=ra(g,p,v,n),z=Ee(N));const st=rt*z;b[I]=m[0]*N,b[I+1]=m[1]*st,b[I+2]=m[2]*st,p<tt&&(X+=j*st,K++)}}const q=X/Math.max(1,K),Q=q>ce*M?(ce*M/q)**We:1,D=f;let Z=0,et=0,J=0;for(let v=0;v<n;v++)for(let w=0;w<s;w++){const p=(v*s+w)*4,I=b[p],W=b[p+1]*Q,Y=b[p+2]*Q;Jt(I,W,Y,y);let O=V(y[0]),L=V(y[1]),B=V(y[2]),j=Ct*O+Rt*L+St*B,rt=1;if(j>l){const N=l/j;O*=N,L*=N,B*=N,j=l,rt=wt(N)}D[p]=O,D[p+1]=L,D[p+2]=B,w<tt&&(Z+=j,et+=ro(I,W,Y)*rt,J++)}const Ho=Z/Math.max(1,J),Xt=et/Math.max(1,J),xe=Math.min(1,Yt.mean*a/Math.max(ct,Ho),(Ye*M/Math.max(ct,Xt))**3);return la(D,xe,c,t,r,i,c.seam?ia(t,c):null)}function sa(t,e){const{rw:o,rh:a,ex:r,tintLab:i,s3:c,ycap:s}=e,[n,u]=e.rect,x=t.data,g=o*a,M=[0,0,0],l=new Float32Array(g*3),m=new Float32Array(g*2);for(let h=0;h<a;h++)for(let f=0;f<o;f++){const d=((u+h)*t.w+n+f)*4,E=h*o+f;gt(H[x[d]],H[x[d+1]],H[x[d+2]],M);const C=M[0];wn(C,M[1],M[2],r,i,c,s,M),l[E*3]=M[0],l[E*3+1]=M[1],l[E*3+2]=M[2],m[E*2]=C,m[E*2+1]=M[0]}const y=vt(m,o,a,e.local,2),_=new Float32Array(g*4),A=[0,0,0];for(let h=0;h<g;h++){const f=Math.max(ie,y[h*2+1]+Qe*(m[h*2]-y[h*2])),d=Math.min(Je,Math.max(Ze,Math.sqrt(f/Math.max(l[h*3],ie))));co(f,l[h*3+1]*d,l[h*3+2]*d,c,s,M),_[h*4+3]=Math.sqrt(M[1]*M[1]+M[2]*M[2]),Jt(M[0],M[1],M[2],A),_[h*4]=V(A[0]),_[h*4+1]=V(A[1]),_[h*4+2]=V(A[2])}return _}function ia(t,e){const{h:o,px:a,frame:r,rw:i,rh:c}=e,{x0:s,x1:n,soft:u,ew:x}=e.seam,[g,M]=e.rect,l=new Float32Array(x*c*3);for(let h=0;h<c;h++)for(let f=0;f<x;f++){const d=((M+h)*t.w+g+f)*4,E=(h*x+f)*3;l[E]=H[t.data[d]],l[E+1]=H[t.data[d+1]],l[E+2]=H[t.data[d+2]]}const m=vt(l,x,c,u,3),y=Math.max(0,n-s),_=new Float32Array(y*o*3),A=new Float32Array(y*o);for(let h=0;h<o;h++){const f=me((h+.5-r.y)/r.h);for(let d=s;d<n;d++){const E=(r.x-(d+.5))*a,C=E>0?1-nt(E/Nt):1-nt((-E-vo*bt)/bt);if(C<=0)continue;const b=h*y+(d-s);Jn(m,x,c,3,Math.abs(E)/a/r.w*i/x,f,_,b*3),_[b*3]*=Ht,_[b*3+1]*=Ht,_[b*3+2]*=Ht,A[b]=C}}return{x0:s,w:y,light:_,alpha:A}}function la(t,e,o,a,r,i,c){const{w:s,h:n,px:u,frame:x,shown:g}=o,[M,l]=Co(r),m=new Uint8ClampedArray(s*n*4),y=i?new Float32Array(s*n*3):null,_=i?Math.ceil(3*i.sigma)+2:0,A=i&&c?Math.max(0,Math.floor(x.x-(le+ue)/u-_)&-2):0,h=i&&c?Math.min(s,c.x0+c.w+_):0,f=h-A,d=f>0?new Float32Array(f*n*3):null;for(let U=0;U<n;U++)for(let R=0;R<s;R++){const $=U*s+R,S=$*4,tt=$*3;let G=t[S]*e,X=t[S+1]*e,K=t[S+2]*e,q=ft(G),Q=ft(X),D=ft(K);y&&(y[tt]=q,y[tt+1]=Q,y[tt+2]=D);const Z=c&&R>=c.x0&&R<c.x0+c.w?U*c.w+(R-c.x0):-1,et=Z>=0?c.alpha[Z]:0;if(et>0&&(G+=(c.light[Z*3]-G)*et,X+=(c.light[Z*3+1]-X)*et,K+=(c.light[Z*3+2]-K)*et,q=ft(G),Q=ft(X),D=ft(K)),d&&R>=A&&R<h){const J=(U*f+R-A)*3;d[J]=q,d[J+1]=Q,d[J+2]=D}m[S]=Math.floor(q*255+yt(S,M)),m[S+1]=Math.floor(Q*255+yt(S+1,M)),m[S+2]=Math.floor(D*255+yt(S+2,M)),m[S+3]=255}const E=x&&!o.immersive?mt({data:m,w:s,h:n},[Math.min(s-1,Math.ceil(x.x)),0,s,n]):mt(a,g),C=Math.ceil(s/2),b=Math.ceil(n/2);let k=null;if(i){const U={...i,sigma:i.sigma/2},R=wo(To(y,s,n),C,b,U);if(d){const $=Math.ceil(f/2),S=A/2,tt=wo(To(d,f,n),$,b,U);for(let G=0;G<$&&S+G<C;G++){const X=ua(x,u,S+G);if(!(X<=0))for(let K=0;K<b;K++){const q=(K*C+S+G)*3,Q=(K*$+G)*3;for(let D=0;D<3;D++)R[q+D]+=(tt[Q+D]-R[q+D])*X}}}k=fa(R,C,b,l)}return{rgba:m,frost:k,frostW:C,frostH:b,tone:k?io(Dn(k,C,b)):null,gain:e,grey:o.grey,light:E}}function ua(t,e,o){const a=(t.x-2*(o+.5))*e-le;return 1-nt(a/ue)}function To(t,e,o){const a=Math.ceil(e/2),r=Math.ceil(o/2),i=new Float32Array(a*r*3);for(let c=0;c<r;c++){const s=2*c,n=Math.min(o-1,s+1);for(let u=0;u<a;u++){const x=2*u,g=Math.min(e-1,x+1),M=(c*a+u)*3;for(let l=0;l<3;l++)i[M+l]=(t[(s*e+x)*3+l]+t[(s*e+g)*3+l]+t[(n*e+x)*3+l]+t[(n*e+g)*3+l])/4}}return i}function Fo(t){return[ht+(1-ht)*t,xt-xt*t,Mt-Mt*t,ht-ht*t,xt+(1-xt)*t,Mt-Mt*t,ht-ht*t,xt-xt*t,Mt+(1-Mt)*t]}function wo(t,e,o,a){const r=vt(t,e,o,a.sigma),i=Fo(a.saturate),c=a.brightness;for(let s=0;s<e*o;s++){const n=s*3,u=r[n],x=r[n+1],g=r[n+2];for(let M=0;M<3;M++)r[n+M]=V(V(i[M*3]*u+i[M*3+1]*x+i[M*3+2]*g)*c)}return r}function fa(t,e,o,a){const r=new Uint8ClampedArray(e*o*4);for(let i=0;i<e*o;i++){const c=i*3,s=i*4;r[s]=Math.floor(t[c]*255+yt(s,a)),r[s+1]=Math.floor(t[c+1]*255+yt(s+1,a)),r[s+2]=Math.floor(t[c+2]*255+yt(s+2,a)),r[s+3]=255}return r}function ma(t){const e=new Uint8ClampedArray(64);for(let o=0;o<e.length;o+=4)e[o]=Math.round(t[0]*255),e[o+1]=Math.round(t[1]*255),e[o+2]=Math.round(t[2]*255),e[o+3]=255;return{data:e,w:4,h:4}}const Ot=8,Io=t=>{const e=String(t);return/[.e]/.test(e)?e:`${e}.0`};function ha(){const t=[];for(const[e,o]of Object.entries(zt.floats))t.push(`#define ${e} ${Io(o)}`);for(const[e,o]of Object.entries(zt.ints))t.push(`#define ${e} ${o}`);for(const[e,o]of Object.entries(zt.uints))t.push(`#define ${e} ${o>>>0}u`);for(const[e,o]of Object.entries(zt.mat3)){const a=[0,1,2].flatMap(r=>[o[r],o[3+r],o[6+r]]);t.push(`#define ${e} mat3(${a.map(Io).join(", ")})`)}return t.push(`#define BLOCK ${Ot}`),t.join(`
`)}const xa=`#version 300 es
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`,Ma=`
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
`,Po=`
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
`,ko=`
uniform sampler2D uSumC;
uniform float uS3;
uniform float uYcap;

float kcOf() {
  vec4 s = texelFetch(uSumC, ivec2(0), 0);
  float meanC = s.x / max(1.0, s.y);
  return meanC > C_MEAN * uS3 ? pow(C_MEAN * uS3 / meanC, C_BUDGET_EXP) : 1.0;
}
`,Kt={box:`
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
}`,grade:`${Po}
uniform sampler2D uSrc;
uniform ivec2 uOrigin;
void main() {
  ivec2 p = ivec2(gl_FragCoord.xy);
  vec3 lab = labOfLinear(linOf3(texelFetch(uSrc, uOrigin + p, 0).rgb));
  outColor = vec4(gradeInto(lab), lab.x);
}`,detail:`${Po}
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
}`,measureYP:`${ko}
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
}`},Bo=`${ko}
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
`;Kt.haze=`${Bo}
uniform uint uKey;
uniform int uW;
uniform int uH;
void main() {
  ivec2 p = ivec2(int(gl_FragCoord.x), uH - 1 - int(gl_FragCoord.y));
  vec3 e = encOf3(withSeam(hazeLinear(p, kcOf(), gainOf()), p));
  int i = (p.y * uW + p.x) * 4;
  outColor = vec4(floor(e.r * 255.0 + noiseAt(i, uKey)), floor(e.g * 255.0 + noiseAt(i + 1, uKey)), floor(e.b * 255.0 + noiseAt(i + 2, uKey)), 255.0) / 255.0;
}`,Kt.half=`${Bo}
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
}`,Kt.frost=`
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
vec3 frosted(vec3 b) { return clamp(clamp(uSat * b, 0.0, 1.0) * uBright, 0.0, 1.0); }
void main() {
  ivec2 p = ivec2(int(gl_FragCoord.x), uH - 1 - int(gl_FragCoord.y));
  vec3 v = frosted(texelFetch(uFrost, p, 0).rgb);
  if (uSpill) {
    float k = 1.0 - smooth01(((uFrame.x - 2.0 * (float(p.x) + 0.5)) * uPx - GAP) / SPILL);
    if (k > 0.0) v += (frosted(texelFetch(uFrostLit, p, 0).rgb) - v) * k;
  }
  int i = (p.y * uW + p.x) * 4;
  outColor = vec4(floor(v.r * 255.0 + noiseAt(i, uKey)), floor(v.g * 255.0 + noiseAt(i + 1, uKey)), floor(v.b * 255.0 + noiseAt(i + 2, uKey)), 255.0) / 255.0;
}`;function ya(){const t=`#version 300 es
precision highp float;
precision highp int;
precision highp sampler2D;
${ha()}
${Ma}`;return Object.fromEntries(Object.entries(Kt).map(([e,o])=>[e,t+o]))}function da(){if(typeof OffscreenCanvas!="function")return null;const t=new OffscreenCanvas(1,1),e=t.getContext("webgl2",{alpha:!1,antialias:!1,depth:!1,stencil:!1,preserveDrawingBuffer:!1});if(!e)return null;let o=null,a=null,r=!1;function i(){if(!e.getExtension("EXT_color_buffer_float"))throw new Error("no float render targets");e.disable(e.DITHER);const l=c(e.VERTEX_SHADER,xa);o={};for(const[m,y]of Object.entries(ya())){const _=c(e.FRAGMENT_SHADER,y),A=e.createProgram();if(e.attachShader(A,l),e.attachShader(A,_),e.linkProgram(A),e.deleteShader(_),!e.getProgramParameter(A,e.LINK_STATUS))throw new Error(`haze ${m}: ${e.getProgramInfoLog(A)}`);const h={};for(let f=0;f<e.getProgramParameter(A,e.ACTIVE_UNIFORMS);f++){const d=e.getActiveUniform(A,f);h[d.name]={loc:e.getUniformLocation(A,d.name),type:d.type}}o[m]={p:A,uniforms:h}}e.deleteShader(l),a=e.createFramebuffer()}function c(l,m){const y=e.createShader(l);if(e.shaderSource(y,m),e.compileShader(y),!e.getShaderParameter(y,e.COMPILE_STATUS))throw new Error(`haze shader: ${e.getShaderInfoLog(y)}`);return y}t.addEventListener("webglcontextlost",l=>{l.preventDefault(),r=!0}),t.addEventListener("webglcontextrestored",()=>{try{i(),r=!1}catch{}}),i();let s=[];function n(l,m,y=null,_=!1){const A=e.createTexture();return e.bindTexture(e.TEXTURE_2D,A),e.texParameteri(e.TEXTURE_2D,e.TEXTURE_MIN_FILTER,e.NEAREST),e.texParameteri(e.TEXTURE_2D,e.TEXTURE_MAG_FILTER,e.NEAREST),e.texParameteri(e.TEXTURE_2D,e.TEXTURE_WRAP_S,e.CLAMP_TO_EDGE),e.texParameteri(e.TEXTURE_2D,e.TEXTURE_WRAP_T,e.CLAMP_TO_EDGE),_?e.texImage2D(e.TEXTURE_2D,0,e.RGBA8,l,m,0,e.RGBA,e.UNSIGNED_BYTE,y):e.texImage2D(e.TEXTURE_2D,0,e.RGBA32F,l,m,0,e.RGBA,e.FLOAT,y),s.push(A),{t:A,w:l,h:m}}function u(l,m,y){const{p:_,uniforms:A}=o[l];e.useProgram(_);let h=0;for(const[f,d]of Object.entries(y)){const E=A[f];if(E)switch(E.type){case e.SAMPLER_2D:e.activeTexture(e.TEXTURE0+h),e.bindTexture(e.TEXTURE_2D,d.t),e.uniform1i(E.loc,h++);break;case e.FLOAT:e.uniform1f(E.loc,d);break;case e.FLOAT_VEC2:e.uniform2fv(E.loc,d);break;case e.FLOAT_VEC3:e.uniform3fv(E.loc,d);break;case e.FLOAT_VEC4:e.uniform4fv(E.loc,d);break;case e.INT:e.uniform1i(E.loc,d);break;case e.BOOL:e.uniform1i(E.loc,d?1:0);break;case e.INT_VEC2:e.uniform2iv(E.loc,d);break;case e.UNSIGNED_INT:e.uniform1ui(E.loc,d);break;case e.FLOAT_MAT3:e.uniformMatrix3fv(E.loc,!1,d);break}}m?(e.bindFramebuffer(e.FRAMEBUFFER,a),e.framebufferTexture2D(e.FRAMEBUFFER,e.COLOR_ATTACHMENT0,e.TEXTURE_2D,m.t,0),e.viewport(0,0,m.w,m.h)):(e.bindFramebuffer(e.FRAMEBUFFER,null),e.viewport(0,0,t.width,t.height)),e.drawArrays(e.TRIANGLES,0,3)}function x(l,m){const y=uo(m),_=n(l.w,l.h),A=n(l.w,l.h);let h=l;for(let f=0;f<3;f++)u("box",_,{uTex:h,uDir:[1,0],uR:y}),u("box",A,{uTex:_,uDir:[0,1],uR:y}),h=A;return A}function g(l,m,y,_){let A=n(Math.ceil(m/Ot),Math.ceil(y/Ot));for(u(l,A,_);A.w>1||A.h>1;){const h=n(Math.ceil(A.w/Ot),Math.ceil(A.h/Ot));u("sum",h,{uTex:A}),A=h}return A}function M(l,m,y,{scale:_=1,seed:A=1,frost:h=null}={}){if(r||e.isContextLost())return null;const f=So(l,m,y,_),{w:d,h:E,rw:C,rh:b,frame:k,ex:U}=f,[R,$]=Co(A);try{t.width=d,t.height=E;const S=n(l.w,l.h,new Uint8Array(l.data.buffer,l.data.byteOffset,l.data.length),!0),tt=[f.rect[0],f.rect[1]],G={uEx:[U.lo,U.mid,U.hi,U.M],uTint:f.tintLab,uS3:f.s3,uYcap:f.ycap},X=n(C,b);u("grade",X,{...G,uSrc:S,uOrigin:tt});const K=n(C,b);u("detail",K,{...G,uGlab:X,uBase:x(X,f.local)});const q=k?[k.x,k.y,k.w,k.h]:[0,0,1,1],Q=f.cols?n(d,1,f.cols):n(1,1,new Float32Array(4)),D=f.closed?[f.closed.ox,f.closed.oy,f.closed.k]:[0,0,1],Z=n(d,E);u("layout",Z,{uGraded:K,uCols:Q,uFramed:!!k,uFrame:q,uClosed:D});const et=x(Z,f.lens),J=n(d,E);u("mix",J,{uLensed:et,uSofter:f.cover?x(Z,f.softer):et,uCols:Q,uFramed:!!k,uCover:f.cover,uFrame:q,uCorner:f.corner,uPool:f.pool?[f.pool.cy,f.pool.span]:[0,1],uBar:f.closed?[f.closed.cx,f.closed.radius]:[0,1],uH:E});const Xt={uSumC:g("measureC",d,E,{uLab:J,uMeasureTo:f.measureTo}),uS3:f.s3,uYcap:f.ycap},xe=g("measureYP",d,E,{...Xt,uLab:J,uMeasureTo:f.measureTo});let v=et;if(f.seam){const O=n(f.seam.ew,b);u("raw",O,{uSrc:S,uOrigin:tt}),v=x(O,f.seam.soft)}const w={...Xt,uLab:J,uSumYP:xe,uEdge:v,uScale:f.scale,uPx:f.px,uSeam:!!f.seam,uSeamX:f.seam?[f.seam.x0,f.seam.x1]:[0,0],uFrame:q,uRw:C,uW:d,uH:E};u("haze",null,{...w,uKey:R});let p;if(k&&!f.immersive){const O=Math.min(d-1,Math.ceil(k.x)),L=d-O,B=new Uint8Array(L*E*4);e.readPixels(O,0,L,E,e.RGBA,e.UNSIGNED_BYTE,B),p=mt({data:B,w:L,h:E},[0,0,L,E],!0)}else p=mt(l,f.shown);const I=t.transferToImageBitmap();let W=null,Y=null;if(h){const O=Math.ceil(d/2),L=Math.ceil(E/2),B=h.sigma/2,j=n(O,L);u("half",j,{...w,uLit:!1});const rt=x(j,B);let N=rt;if(f.seam){const P=n(O,L);u("half",P,{...w,uLit:!0}),N=x(P,B)}t.width=O,t.height=L;const z=Fo(h.saturate);u("frost",null,{uFrost:rt,uFrostLit:N,uSpill:!!f.seam,uSat:[z[0],z[3],z[6],z[1],z[4],z[7],z[2],z[5],z[8]],uBright:h.brightness,uFrame:q,uPx:f.px,uKey:$,uW:O,uH:L});const st=new Uint8Array(Gt.length*L*4);Gt.forEach((P,Lt)=>{e.readPixels(Math.min(O-1,Math.floor(O*P)),0,1,L,e.RGBA,e.UNSIGNED_BYTE,st.subarray(Lt*L*4,(Lt+1)*L*4))}),Y=io(st),W=t.transferToImageBitmap()}return e.isContextLost()?(I.close(),W==null||W.close(),null):{bitmap:I,frost:W,tone:Y,grey:f.grey,light:p}}finally{for(const S of s)e.deleteTexture(S);s=[],t.width=t.height=1}}return{render:M,context:e}}const he=new OffscreenCanvas(it,it),qt=he.getContext("2d");qt.imageSmoothingQuality="high";const Wt=new OffscreenCanvas(it,it).getContext("2d",{willReadFrequently:!0}),dt=new OffscreenCanvas(1,1),Aa=dt.getContext("2d");let At=null,T=null,F=null,_a=1;function Go(t){const e=t.displayWidth||t.width,o=t.displayHeight||t.height;if(!e||!o)return;const a=it/Math.max(e,o),r=Math.max(8,Math.round(e*a)),i=Math.max(8,Math.round(o*a));qt.clearRect(0,0,r,i),qt.drawImage(t,0,0,r,i);try{return Wt.clearRect(0,0,r,i),Wt.drawImage(he,0,0,r,i,0,0,r,i),{data:Wt.getImageData(0,0,r,i).data,w:r,h:i}}catch{return he.width=it,Wt.canvas.width=it,qt.imageSmoothingQuality="high",null}}const Do=(t,e)=>({top:Math.max(t.top,e.top),bottom:Math.max(t.bottom,e.bottom)}),ga={top:1,bottom:1};function Ea(t,e,o){const a=ca(t,e.geo,e.tint,o),r=(i,c,s)=>((dt.width!==c||dt.height!==s)&&(dt.width=c,dt.height=s),Aa.putImageData(new ImageData(i,c,s),0,0),dt.transferToImageBitmap());return{bitmap:r(a.rgba,e.geo.w,e.geo.h),frost:a.frost?r(a.frost,a.frostW,a.frostH):null,tone:a.tone,grey:a.grey,light:a.light}}function pa(t,e,o){if(!At)return null;try{return At.render(t,e.geo,e.tint,o)}catch(a){return console.warn("haze: the GPU renderer failed; drawing on the CPU from now on.",a),At=!1,null}}function No(t,e,o=null,a=void 0){const r=performance.now(),i=e??ma(t.tint),c={scale:t.scale,seed:_a++,frost:t.frost??null},s=pa(i,t,c),n=s??Ea(i,t,c),u=e?o?Do(o,n.light):n.light:ga;postMessage({type:"haze",seq:t.seq,key:t.key,fade:t.fade,bitmap:n.bitmap,frost:n.frost,tone:n.tone,grey:n.grey,light:u,distance:a,gpu:!!s,ms:performance.now()-r},n.frost?[n.bitmap,n.frost]:[n.bitmap])}onmessage=({data:t})=>{var e;switch(t.type){case"init":{if(t.cpu){At=!1;break}try{At=da()??!1}catch(o){console.warn("haze: no GPU renderer; drawing on the CPU.",o),At=!1}break}case"render":{if(t.source!==void 0){const o=t.source?Go(t.source):null;if((e=t.source)==null||e.close(),o===void 0)break;if(o&&t.video&&!t.allowBlank&&Pn(o)){postMessage({type:"blank",seq:t.seq});break}(T==null?void 0:T.key)!==t.key&&(F=null),T={key:t.key,src:o}}if((T==null?void 0:T.key)!==t.key)break;No(t,T.src,(F==null?void 0:F.key)===t.key?F.light:null);break}case"sample":{const o=(T==null?void 0:T.key)===t.key&&T.src?Go(t.frame):null;if(t.frame.close(),!o)break;const a=mt(o,Bt(o,t.geo).shown);(F==null?void 0:F.key)!==t.key&&(F={key:t.key,frames:[T.src],light:mt(T.src,Bt(T.src,t.geo).shown)});const r=F.light;F.frames.push(o),F.light=Do(r,a),(F.light.top>r.top+.02||F.light.bottom>r.bottom+.02)&&postMessage({type:"light",key:t.key,light:F.light});break}case"settle":{if(!F||F.key!==t.key||(T==null?void 0:T.key)!==t.key){postMessage({type:"done",seq:t.seq,key:t.key,light:null});break}const{frames:o,light:a}=F;F=null;const r=o.map(s=>Un(s,Bt(s,t.geo).rect)),i=zn(r),c=i===0?0:lo(r[i],r[0]);c>Kn?(T={key:t.key,src:o[i]},No(t,T.src,a,c)):postMessage({type:"done",seq:t.seq,key:t.key,light:a,distance:c});break}}}})();
