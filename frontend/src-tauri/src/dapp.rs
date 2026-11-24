// ============================================================
//  SmartNet // dapp.rs
//
//  Web3 provider bridge (`window.smartholdem`). An initialization script
//  is injected into every embedded dApp webview, exposing a provider whose
//  calls (connect / sendTransaction / sign …) reach `dapp_provider_request`
//  here. Approval-gated methods emit `incoming-tx-request` to the host shell
//  (MetaMask-style); the shell's Approve/Reject path then emits `dapp-response`
//  (via `emit_dapp_response`) which the injected bridge resolves back to the
//  dApp's pending Promise. The connect handshake is whitelisted per origin.
//
//  Author: TechnoL0g
//  Created: 2026-06-18
// ============================================================

use crate::SESSION_SEED;
use crate::sth_node;
use tauri::Manager;


/// Cross-platform Web Crypto fallback. On platforms/schemes where `sth://` is
/// NOT a secure context (e.g. WKWebView/webkit2gtk custom schemes) the native
/// `window.crypto.subtle` is undefined, breaking dApps that hash or verify
/// signatures. This injects a pure-JS SubtleCrypto implementing:
///   - digest        : SHA-1/256/384/512
///   - importKey     : HMAC (raw/jwk) + ECDSA/ECDH P-256/P-384 (jwk/raw) + AES-GCM (raw/jwk)
///   - sign / verify : HMAC + ECDSA (deterministic nonce per RFC 6979)
///   - deriveBits / deriveKey : ECDH (P-256/P-384) -> raw / AES-GCM key
///   - encrypt / decrypt      : AES-GCM (128/192/256, authenticated)
///   - getRandomValues + randomUUID : HMAC-DRBG seeded with OS entropy (host-injected)
/// It is installed ONLY when the native API is missing, so secure-context
/// platforms (Windows: `sth://` maps to `*.sth.localhost`, a secure context)
/// keep the real implementation.
/// `__STH_CSPRNG_SEED_HEX__` is replaced per-webview by `crypto_polyfill_script()`.
pub(crate) const CRYPTO_POLYFILL_JS: &str = r#"
(function () {
  try {
    var g = window;
    if (!g.crypto) { try { g.crypto = {}; } catch (e) {} }
    var C = g.crypto;
    // CSPRNG (getRandomValues/randomUUID) + full SubtleCrypto are installed below,
    // after the hash/HMAC primitives they depend on are defined. Bail out early on
    // secure-context platforms that already expose a real crypto.subtle.
    if (C.subtle && typeof C.subtle.digest === 'function') return; // native secure context

    function pad(msg, blk, lenBytes) {
      var l=msg.length, withOne=l+1, k=((blk-lenBytes)-withOne%blk+blk)%blk, total=withOne+k+lenBytes;
      var m=new Uint8Array(total); m.set(msg); m[l]=0x80; return m;
    }
    function sha1(msg){
      function R(n,x){return (x<<n)|(x>>>(32-n));}
      var H=[0x67452301,0xEFCDAB89,0x98BADCFE,0x10325476,0xC3D2E1F0];
      var bit=msg.length*8, m=pad(msg,64,8), total=m.length;
      var hi=Math.floor(bit/0x100000000),lo=bit>>>0;
      m[total-8]=(hi>>>24)&255;m[total-7]=(hi>>>16)&255;m[total-6]=(hi>>>8)&255;m[total-5]=hi&255;
      m[total-4]=(lo>>>24)&255;m[total-3]=(lo>>>16)&255;m[total-2]=(lo>>>8)&255;m[total-1]=lo&255;
      var w=new Uint32Array(80);
      for(var i=0;i<total;i+=64){
        for(var t=0;t<16;t++){var j=i+t*4;w[t]=(m[j]<<24)|(m[j+1]<<16)|(m[j+2]<<8)|m[j+3];}
        for(var t=16;t<80;t++)w[t]=R(1,w[t-3]^w[t-8]^w[t-14]^w[t-16]);
        var a=H[0],b=H[1],c=H[2],d=H[3],e=H[4];
        for(var t=0;t<80;t++){var f,kk;
          if(t<20){f=(b&c)|(~b&d);kk=0x5A827999;}else if(t<40){f=b^c^d;kk=0x6ED9EBA1;}
          else if(t<60){f=(b&c)|(b&d)|(c&d);kk=0x8F1BBCDC;}else{f=b^c^d;kk=0xCA62C1D6;}
          var tm=(R(5,a)+f+e+kk+w[t])>>>0; e=d;d=c;c=R(30,b);b=a;a=tm;}
        H[0]=(H[0]+a)>>>0;H[1]=(H[1]+b)>>>0;H[2]=(H[2]+c)>>>0;H[3]=(H[3]+d)>>>0;H[4]=(H[4]+e)>>>0;
      }
      var o=new Uint8Array(20);
      for(var i=0;i<5;i++){o[i*4]=(H[i]>>>24)&255;o[i*4+1]=(H[i]>>>16)&255;o[i*4+2]=(H[i]>>>8)&255;o[i*4+3]=H[i]&255;}
      return o;
    }
    var K256=[0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2];
    function sha256(msg){
      function R(n,x){return (x>>>n)|(x<<(32-n));}
      var H=[0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19];
      var bit=msg.length*8, m=pad(msg,64,8), total=m.length;
      var hi=Math.floor(bit/0x100000000),lo=bit>>>0;
      m[total-8]=(hi>>>24)&255;m[total-7]=(hi>>>16)&255;m[total-6]=(hi>>>8)&255;m[total-5]=hi&255;
      m[total-4]=(lo>>>24)&255;m[total-3]=(lo>>>16)&255;m[total-2]=(lo>>>8)&255;m[total-1]=lo&255;
      var w=new Uint32Array(64);
      for(var i=0;i<total;i+=64){
        for(var t=0;t<16;t++){var j=i+t*4;w[t]=(m[j]<<24)|(m[j+1]<<16)|(m[j+2]<<8)|m[j+3];}
        for(var t=16;t<64;t++){var s0=R(7,w[t-15])^R(18,w[t-15])^(w[t-15]>>>3),s1=R(17,w[t-2])^R(19,w[t-2])^(w[t-2]>>>10);w[t]=(w[t-16]+s0+w[t-7]+s1)>>>0;}
        var a=H[0],b=H[1],c=H[2],d=H[3],e=H[4],f=H[5],G=H[6],h=H[7];
        for(var t=0;t<64;t++){
          var S1=R(6,e)^R(11,e)^R(25,e),ch=(e&f)^(~e&G),t1=(h+S1+ch+K256[t]+w[t])>>>0;
          var S0=R(2,a)^R(13,a)^R(22,a),mj=(a&b)^(a&c)^(b&c),t2=(S0+mj)>>>0;
          h=G;G=f;f=e;e=(d+t1)>>>0;d=c;c=b;b=a;a=(t1+t2)>>>0;}
        H[0]=(H[0]+a)>>>0;H[1]=(H[1]+b)>>>0;H[2]=(H[2]+c)>>>0;H[3]=(H[3]+d)>>>0;H[4]=(H[4]+e)>>>0;H[5]=(H[5]+f)>>>0;H[6]=(H[6]+G)>>>0;H[7]=(H[7]+h)>>>0;
      }
      var o=new Uint8Array(32);
      for(var i=0;i<8;i++){o[i*4]=(H[i]>>>24)&255;o[i*4+1]=(H[i]>>>16)&255;o[i*4+2]=(H[i]>>>8)&255;o[i*4+3]=H[i]&255;}
      return o;
    }
    var Kh="428a2f98d728ae22 7137449123ef65cd b5c0fbcfec4d3b2f e9b5dba58189dbbc 3956c25bf348b538 59f111f1b605d019 923f82a4af194f9b ab1c5ed5da6d8118 d807aa98a3030242 12835b0145706fbe 243185be4ee4b28c 550c7dc3d5ffb4e2 72be5d74f27b896f 80deb1fe3b1696b1 9bdc06a725c71235 c19bf174cf692694 e49b69c19ef14ad2 efbe4786384f25e3 0fc19dc68b8cd5b5 240ca1cc77ac9c65 2de92c6f592b0275 4a7484aa6ea6e483 5cb0a9dcbd41fbd4 76f988da831153b5 983e5152ee66dfab a831c66d2db43210 b00327c898fb213f bf597fc7beef0ee4 c6e00bf33da88fc2 d5a79147930aa725 06ca6351e003826f 142929670a0e6e70 27b70a8546d22ffc 2e1b21385c26c926 4d2c6dfc5ac42aed 53380d139d95b3df 650a73548baf63de 766a0abb3c77b2a8 81c2c92e47edaee6 92722c851482353b a2bfe8a14cf10364 a81a664bbc423001 c24b8b70d0f89791 c76c51a30654be30 d192e819d6ef5218 d69906245565a910 f40e35855771202a 106aa07032bbd1b8 19a4c116b8d2d0c8 1e376c085141ab53 2748774cdf8eeb99 34b0bcb5e19b48a8 391c0cb3c5c95a63 4ed8aa4ae3418acb 5b9cca4f7763e373 682e6ff3d6b2b8a3 748f82ee5defb2fc 78a5636f43172f60 84c87814a1f0ab72 8cc702081a6439ec 90befffa23631e28 a4506cebde82bde9 bef9a3f7b2c67915 c67178f2e372532b ca273eceea26619c d186b8c721c0c207 eada7dd6cde0eb1e f57d4f7fee6ed178 06f067aa72176fba 0a637dc5a2c898a6 113f9804bef90dae 1b710b35131c471b 28db77f523047d84 32caab7b40c72493 3c9ebe0a15c9bebc 431d67c49c100d4c 4cc5d4becb3e42b6 597f299cfc657e2a 5fcb6fab3ad6faec 6c44198c4a475817".split(' ');
    var K512=Kh.map(function(x){return BigInt('0x'+x);});
    function sha512(msg, is384){
      var MASK=(1n<<64n)-1n;
      function R(x,n){return ((x>>n)|(x<<(64n-n)))&MASK;}
      var H=(is384?["cbbb9d5dc1059ed8","629a292a367cd507","9159015a3070dd17","152fecd8f70e5939","67332667ffc00b31","8eb44a8768581511","db0c2e0d64f98fa7","47b5481dbefa4fa4"]:["6a09e667f3bcc908","bb67ae8584caa73b","3c6ef372fe94f82b","a54ff53a5f1d36f1","510e527fade682d1","9b05688c2b3e6c1f","1f83d9abfb41bd6b","5be0cd19137e2179"]).map(function(x){return BigInt('0x'+x);});
      var bit=BigInt(msg.length)*8n, m=pad(msg,128,16), total=m.length;
      for(var i=0;i<8;i++) m[total-1-i]=Number((bit>>BigInt(8*i))&0xffn);
      var w=new Array(80);
      for(var off=0;off<total;off+=128){
        for(var t=0;t<16;t++){var v=0n;for(var b2=0;b2<8;b2++)v=(v<<8n)|BigInt(m[off+t*8+b2]);w[t]=v;}
        for(var t=16;t<80;t++){var s0=R(w[t-15],1n)^R(w[t-15],8n)^(w[t-15]>>7n),s1=R(w[t-2],19n)^R(w[t-2],61n)^(w[t-2]>>6n);w[t]=(w[t-16]+s0+w[t-7]+s1)&MASK;}
        var a=H[0],b=H[1],c=H[2],d=H[3],e=H[4],f=H[5],G=H[6],h=H[7];
        for(var t=0;t<80;t++){
          var S1=R(e,14n)^R(e,18n)^R(e,41n),ch=(e&f)^((~e&MASK)&G),t1=(h+S1+ch+K512[t]+w[t])&MASK;
          var S0=R(a,28n)^R(a,34n)^R(a,39n),mj=(a&b)^(a&c)^(b&c),t2=(S0+mj)&MASK;
          h=G;G=f;f=e;e=(d+t1)&MASK;d=c;c=b;b=a;a=(t1+t2)&MASK;}
        H[0]=(H[0]+a)&MASK;H[1]=(H[1]+b)&MASK;H[2]=(H[2]+c)&MASK;H[3]=(H[3]+d)&MASK;H[4]=(H[4]+e)&MASK;H[5]=(H[5]+f)&MASK;H[6]=(H[6]+G)&MASK;H[7]=(H[7]+h)&MASK;
      }
      var n=is384?6:8, o=new Uint8Array(n*8);
      for(var i=0;i<n;i++)for(var b2=0;b2<8;b2++)o[i*8+b2]=Number((H[i]>>BigInt(56-8*b2))&0xffn);
      return o;
    }
    function toBytes(d){
      if(d instanceof ArrayBuffer) return new Uint8Array(d.slice(0));
      if(ArrayBuffer.isView(d)) return new Uint8Array(d.buffer.slice(d.byteOffset, d.byteOffset+d.byteLength));
      return new Uint8Array(d);
    }
    function normHash(h){var n=(typeof h==='string'?h:(h&&h.name)||'').toUpperCase(); if(n==='SHA1')n='SHA-1'; if(n==='SHA256')n='SHA-256'; if(n==='SHA384')n='SHA-384'; if(n==='SHA512')n='SHA-512'; return n;}
    function hashFn(n){
      if(n==='SHA-1') return {fn:function(b){return sha1(b);},block:64,len:20};
      if(n==='SHA-256') return {fn:function(b){return sha256(b);},block:64,len:32};
      if(n==='SHA-384') return {fn:function(b){return sha512(b,true);},block:128,len:48};
      if(n==='SHA-512') return {fn:function(b){return sha512(b,false);},block:128,len:64};
      throw new Error('Unsupported hash: '+n);
    }
    function concatBytes(a,b){var o=new Uint8Array(a.length+b.length);o.set(a);o.set(b,a.length);return o;}
    function hmacBytes(hn,key,msg){
      var H=hashFn(hn), bl=H.block;
      if(key.length>bl) key=H.fn(key);
      if(key.length<bl){var k=new Uint8Array(bl);k.set(key);key=k;}
      var ip=new Uint8Array(bl), op=new Uint8Array(bl);
      for(var i=0;i<bl;i++){ip[i]=key[i]^0x36;op[i]=key[i]^0x5c;}
      return H.fn(concatBytes(op,H.fn(concatBytes(ip,msg))));
    }
    function ctEq(a,b){if(a.length!==b.length)return false;var d=0;for(var i=0;i<a.length;i++)d|=a[i]^b[i];return d===0;}
    function b64u(s){s=String(s).replace(/-/g,'+').replace(/_/g,'/');while(s.length%4)s+='=';var bin=atob(s);var o=new Uint8Array(bin.length);for(var i=0;i<bin.length;i++)o[i]=bin.charCodeAt(i);return o;}
    function b2i(b){var x=0n;for(var i=0;i<b.length;i++)x=(x<<8n)|BigInt(b[i]);return x;}
    function i2b(x,len){var o=new Uint8Array(len);for(var i=len-1;i>=0;i--){o[i]=Number(x&0xffn);x>>=8n;}return o;}
    function bitLen(x){var n=0;while(x>0n){x>>=1n;n++;}return n;}
    function modInv(a,m){var or=((a%m)+m)%m,r=m,os=1n,s=0n;while(r!==0n){var q=or/r,t=or-q*r;or=r;r=t;t=os-q*s;os=s;s=t;}if(or!==1n)throw new Error('not invertible');return ((os%m)+m)%m;}
    var CURVES={
      'P-256':{p:BigInt('0xffffffff00000001000000000000000000000000ffffffffffffffffffffffff'),a:BigInt('0xffffffff00000001000000000000000000000000fffffffffffffffffffffffc'),b:BigInt('0x5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b'),Gx:BigInt('0x6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296'),Gy:BigInt('0x4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5'),n:BigInt('0xffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551'),size:32},
      'P-384':{p:BigInt('0xfffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff'),a:BigInt('0xfffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000fffffffc'),b:BigInt('0xb3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef'),Gx:BigInt('0xaa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7'),Gy:BigInt('0x3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f'),n:BigInt('0xffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973'),size:48}
    };
    function ecDouble(P,cu){if(!P||P.y===0n)return null;var p=cu.p;var m=((3n*P.x*P.x+cu.a)%p*modInv(((2n*P.y)%p+p)%p,p))%p;var x=((m*m-2n*P.x)%p+p)%p;var y=((m*(P.x-x)-P.y)%p+p)%p;return {x:x,y:y};}
    function ecAdd(P,Q,cu){if(!P)return Q;if(!Q)return P;var p=cu.p;if(P.x===Q.x){if(((P.y+Q.y)%p+p)%p===0n)return null;return ecDouble(P,cu);}var m=(((Q.y-P.y)%p+p)%p*modInv(((Q.x-P.x)%p+p)%p,p))%p;var x=((m*m-P.x-Q.x)%p+p)%p;var y=((m*(P.x-x)-P.y)%p+p)%p;return {x:x,y:y};}
    function ecMul(k,P,cu){var R=null,N=P;while(k>0n){if(k&1n)R=ecAdd(R,N,cu);N=ecDouble(N,cu);k>>=1n;}return R;}
    function bits2int(b,qlen){var x=b2i(b);var bl=b.length*8;if(bl>qlen)x>>=BigInt(bl-qlen);return x;}
    function bits2octets(b,qlen,n,rlen){var z1=bits2int(b,qlen);var z2=z1>=n?z1-n:z1;return i2b(z2,rlen);}
    function rfc6979k(cu,d,h1,hn){
      var H=hashFn(hn),hlen=H.len,n=cu.n,qlen=bitLen(n),rlen=Math.ceil(qlen/8);
      var V=new Uint8Array(hlen);V.fill(1);var K=new Uint8Array(hlen);
      var x=i2b(d,rlen),ho=bits2octets(h1,qlen,n,rlen);
      K=hmacBytes(hn,K,concatBytes(concatBytes(concatBytes(V,new Uint8Array([0])),x),ho));V=hmacBytes(hn,K,V);
      K=hmacBytes(hn,K,concatBytes(concatBytes(concatBytes(V,new Uint8Array([1])),x),ho));V=hmacBytes(hn,K,V);
      while(true){var T=new Uint8Array(0);while(T.length*8<qlen){V=hmacBytes(hn,K,V);T=concatBytes(T,V);}var k=bits2int(T,qlen);if(k>=1n&&k<n)return k;K=hmacBytes(hn,K,concatBytes(V,new Uint8Array([0])));V=hmacBytes(hn,K,V);}
    }
    function ecdsaSign(cu,d,msg,hn){
      var H=hashFn(hn),h1=H.fn(msg),n=cu.n,qlen=bitLen(n),rlen=Math.ceil(qlen/8);
      var e=bits2int(h1,qlen),G={x:cu.Gx,y:cu.Gy},k=rfc6979k(cu,d,h1,hn);
      var P=ecMul(k,G,cu),r=((P.x%n)+n)%n,s=((modInv(k,n)*((e+r*d)%n))%n+n)%n;
      return concatBytes(i2b(r,rlen),i2b(s,rlen));
    }
    function ecdsaVerify(cu,pub,msg,sig,hn){
      var n=cu.n,qlen=bitLen(n);if(sig.length%2!==0)return false;var rlen=sig.length/2;
      var r=b2i(sig.slice(0,rlen)),s=b2i(sig.slice(rlen));
      if(r<1n||r>=n||s<1n||s>=n)return false;
      var H=hashFn(hn),e=bits2int(H.fn(msg),qlen),w=modInv(s,n),u1=(e*w)%n,u2=(r*w)%n,G={x:cu.Gx,y:cu.Gy};
      var X=ecAdd(ecMul(u1,G,cu),ecMul(u2,pub,cu),cu);if(!X)return false;
      return (((X.x%n)+n)%n)===r;
    }
    function hexToBytes(h){var o=new Uint8Array(h.length/2);for(var i=0;i<o.length;i++)o[i]=parseInt(h.substr(i*2,2),16);return o;}
    function rotl8(x,s){return ((x<<s)|(x>>(8-s)))&0xff;}
    var SBOX=(function(){var p=1,q=1,s=new Uint8Array(256);s[0]=0x63;do{p=(p^(p<<1)^((p&0x80)?0x11b:0))&0xff;q^=q<<1;q^=q<<2;q^=q<<4;q&=0xff;if(q&0x80)q^=0x09;q&=0xff;var x=(q^rotl8(q,1)^rotl8(q,2)^rotl8(q,3)^rotl8(q,4))&0xff;s[p]=(x^0x63)&0xff;}while(p!==1);return s;})();
    var RCON=[0x01,0x02,0x04,0x08,0x10,0x20,0x40,0x80,0x1b,0x36,0x6c,0xd8,0xab,0x4d];
    function xtime(a){return ((a<<1)^((a&0x80)?0x11b:0))&0xff;}
    function gmul(a,b){var r=0;while(b){if(b&1)r^=a;a=xtime(a);b>>=1;}return r&0xff;}
    function AES(key){
      var Nk=key.length/4, Nr=Nk+6; this.Nr=Nr;
      var w=new Uint8Array(16*(Nr+1)); w.set(key); var t=new Uint8Array(4);
      for(var i=Nk;i<4*(Nr+1);i++){
        var k=(i-1)*4; t[0]=w[k];t[1]=w[k+1];t[2]=w[k+2];t[3]=w[k+3];
        if(i%Nk===0){ var tmp=t[0]; t[0]=(SBOX[t[1]]^RCON[i/Nk-1])&0xff; t[1]=SBOX[t[2]]; t[2]=SBOX[t[3]]; t[3]=SBOX[tmp]; }
        else if(Nk>6 && i%Nk===4){ t[0]=SBOX[t[0]];t[1]=SBOX[t[1]];t[2]=SBOX[t[2]];t[3]=SBOX[t[3]]; }
        var pp=(i-Nk)*4, c=i*4; w[c]=w[pp]^t[0]; w[c+1]=w[pp+1]^t[1]; w[c+2]=w[pp+2]^t[2]; w[c+3]=w[pp+3]^t[3];
      }
      this.w=w;
    }
    function aesShiftRows(s){var o=new Uint8Array(16);for(var r=0;r<4;r++)for(var c=0;c<4;c++)o[r+4*c]=s[r+4*((c+r)%4)];return o;}
    function aesMixCols(s){var o=new Uint8Array(16);for(var c=0;c<4;c++){var i=4*c,a0=s[i],a1=s[i+1],a2=s[i+2],a3=s[i+3];o[i]=gmul(a0,2)^gmul(a1,3)^a2^a3;o[i+1]=a0^gmul(a1,2)^gmul(a2,3)^a3;o[i+2]=a0^a1^gmul(a2,2)^gmul(a3,3);o[i+3]=gmul(a0,3)^a1^a2^gmul(a3,2);}return o;}
    AES.prototype.encryptBlock=function(inp){
      var Nr=this.Nr, w=this.w, s=new Uint8Array(16), i;
      for(i=0;i<16;i++) s[i]=inp[i]^w[i];
      for(var r=1;r<Nr;r++){ for(i=0;i<16;i++) s[i]=SBOX[s[i]]; s=aesShiftRows(s); s=aesMixCols(s); for(i=0;i<16;i++) s[i]^=w[r*16+i]; }
      for(i=0;i<16;i++) s[i]=SBOX[s[i]]; s=aesShiftRows(s); for(i=0;i<16;i++) s[i]^=w[Nr*16+i];
      return s;
    };
    function gfmul128(X,Y){var Z=new Uint8Array(16),V=Y.slice(),i,j;for(i=0;i<128;i++){if((X[i>>3]>>(7-(i&7)))&1){for(j=0;j<16;j++)Z[j]^=V[j];}var lsb=V[15]&1;for(j=15;j>0;j--)V[j]=((V[j]>>1)|((V[j-1]&1)<<7))&0xff;V[0]=V[0]>>1;if(lsb)V[0]^=0xe1;}return Z;}
    function ghash(H,data){var Y=new Uint8Array(16);for(var i=0;i<data.length;i+=16){for(var j=0;j<16;j++)Y[j]^=data[i+j];Y=gfmul128(Y,H);}return Y;}
    function incr32(ctr){for(var i=15;i>=12;i--){ctr[i]=(ctr[i]+1)&0xff;if(ctr[i]!==0)break;}}
    function gctr(aes,icb,data){var out=new Uint8Array(data.length),cb=icb.slice();for(var i=0;i<data.length;i+=16){var ks=aes.encryptBlock(cb),n=Math.min(16,data.length-i);for(var j=0;j<n;j++)out[i+j]=data[i+j]^ks[j];incr32(cb);}return out;}
    function pad16(x){var r=(16-(x.length%16))%16;if(r===0)return x;var o=new Uint8Array(x.length+r);o.set(x);return o;}
    function put64(arr,off,bits){var v=bits;for(var i=7;i>=0;i--){arr[off+i]=v&0xff;v=Math.floor(v/256);}}
    function gcmJ0(H,iv){if(iv.length===12){var j=new Uint8Array(16);j.set(iv);j[15]=1;return j;}var p=pad16(iv),blk=new Uint8Array(p.length+16);blk.set(p);put64(blk,p.length+8,iv.length*8);return ghash(H,blk);}
    function gcmBlocks(aad,C){var a=pad16(aad),c=pad16(C),out=new Uint8Array(a.length+c.length+16);out.set(a,0);out.set(c,a.length);put64(out,a.length+c.length,aad.length*8);put64(out,a.length+c.length+8,C.length*8);return out;}
    function gcmEncrypt(aes,iv,plain,aad,tagBytes){var H=aes.encryptBlock(new Uint8Array(16)),J0=gcmJ0(H,iv),J1=J0.slice();incr32(J1);var C=gctr(aes,J1,plain),S=ghash(H,gcmBlocks(aad,C)),T=gctr(aes,J0,S).slice(0,tagBytes);return concatBytes(C,T);}
    function gcmDecrypt(aes,iv,C,aad,T,tagBytes){var H=aes.encryptBlock(new Uint8Array(16)),J0=gcmJ0(H,iv),S=ghash(H,gcmBlocks(aad,C)),T2=gctr(aes,J0,S).slice(0,tagBytes);if(!ctEq(T2,T))return null;var J1=J0.slice();incr32(J1);return gctr(aes,J1,C);}
    function ecdhSecret(cu,d,pub){var S=ecMul(d,pub,cu);if(!S)throw new Error('invalid ECDH public key');return i2b(S.x,cu.size);}
    var subtle = {
      digest: function (algo, data) {
        return new Promise(function (resolve, reject) {
          try {
            var name=normHash(algo);
            var by=toBytes(data), out;
            if(name==='SHA-1') out=sha1(by);
            else if(name==='SHA-256') out=sha256(by);
            else if(name==='SHA-384') out=sha512(by,true);
            else if(name==='SHA-512') out=sha512(by,false);
            else { reject(new Error('Unsupported digest algorithm: '+name)); return; }
            resolve(out.buffer);
          } catch (e) { reject(e); }
        });
      },
      importKey: function (format, keyData, algorithm, extractable, usages) {
        return new Promise(function (resolve, reject) {
          try {
            var alg=String((algorithm&&algorithm.name)||algorithm||'').toUpperCase();
            if(alg==='HMAC'){
              var kb = format==='jwk' ? b64u(keyData.k) : toBytes(keyData);
              resolve({__poly:true,type:'HMAC',hash:normHash(algorithm.hash),key:kb,usages:usages}); return;
            }
            if(alg==='ECDSA'||alg==='ECDH'){
              var crv=(algorithm&&algorithm.namedCurve)||(format==='jwk'&&keyData.crv);
              if(!CURVES[crv]){ reject(new Error('Unsupported curve: '+crv)); return; }
              if(format==='jwk'){
                var d=keyData.d?b2i(b64u(keyData.d)):null;
                resolve({__poly:true,type:alg,curve:crv,pub:{x:b2i(b64u(keyData.x)),y:b2i(b64u(keyData.y))},d:d}); return;
              }
              if(format==='raw'){
                var by=toBytes(keyData), sz=CURVES[crv].size;
                if(by[0]!==0x04){ reject(new Error('only uncompressed EC raw keys supported')); return; }
                resolve({__poly:true,type:alg,curve:crv,pub:{x:b2i(by.slice(1,1+sz)),y:b2i(by.slice(1+sz,1+2*sz))},d:null}); return;
              }
              reject(new Error('Unsupported EC key format: '+format)); return;
            }
            if(alg==='AES-GCM'){
              var akb = format==='jwk' ? b64u(keyData.k) : toBytes(keyData);
              resolve({__poly:true,type:'AES-GCM',key:akb}); return;
            }
            reject(new Error('Unsupported importKey algorithm: '+alg));
          } catch (e) { reject(e); }
        });
      },
      sign: function (algorithm, key, data) {
        return new Promise(function (resolve, reject) {
          try {
            var alg=String((algorithm&&algorithm.name)||algorithm||'').toUpperCase();
            var msg=toBytes(data);
            if(alg==='HMAC'){ resolve(hmacBytes(key.hash,key.key,msg).buffer); return; }
            if(alg==='ECDSA'){
              if(!key.d){ reject(new Error('ECDSA sign requires a private key')); return; }
              resolve(ecdsaSign(CURVES[key.curve],key.d,msg,normHash(algorithm.hash)).buffer); return;
            }
            reject(new Error('Unsupported sign algorithm: '+alg));
          } catch (e) { reject(e); }
        });
      },
      verify: function (algorithm, key, signature, data) {
        return new Promise(function (resolve, reject) {
          try {
            var alg=String((algorithm&&algorithm.name)||algorithm||'').toUpperCase();
            var msg=toBytes(data), sig=toBytes(signature);
            if(alg==='HMAC'){ resolve(ctEq(hmacBytes(key.hash,key.key,msg),sig)); return; }
            if(alg==='ECDSA'){ resolve(ecdsaVerify(CURVES[key.curve],key.pub,msg,sig,normHash(algorithm.hash))); return; }
            reject(new Error('Unsupported verify algorithm: '+alg));
          } catch (e) { reject(e); }
        });
      },
      deriveBits: function (algorithm, key, length) {
        return new Promise(function (resolve, reject) {
          try {
            var alg=String((algorithm&&algorithm.name)||'').toUpperCase();
            if(alg!=='ECDH'){ reject(new Error('Unsupported deriveBits algorithm: '+alg)); return; }
            if(!key.d){ reject(new Error('deriveBits requires a private key')); return; }
            var cu=CURVES[key.curve], secret=ecdhSecret(cu,key.d,algorithm.public.pub);
            var n=(length==null)?cu.size:Math.ceil(length/8);
            resolve(secret.slice(0,n).buffer); return;
          } catch (e) { reject(e); }
        });
      },
      deriveKey: function (algorithm, key, derivedKeyAlgo, extractable, usages) {
        return subtle.deriveBits(algorithm, key, (derivedKeyAlgo&&derivedKeyAlgo.length)||256)
          .then(function (bits) { return subtle.importKey('raw', bits, derivedKeyAlgo, extractable, usages); });
      },
      encrypt: function (algorithm, key, data) {
        return new Promise(function (resolve, reject) {
          try {
            var alg=String((algorithm&&algorithm.name)||'').toUpperCase();
            if(alg!=='AES-GCM'){ reject(new Error('Unsupported encrypt algorithm: '+alg)); return; }
            var aes=new AES(key.key), iv=toBytes(algorithm.iv);
            var aad=algorithm.additionalData?toBytes(algorithm.additionalData):new Uint8Array(0);
            var tagBytes=(algorithm.tagLength!=null?algorithm.tagLength:128)/8;
            resolve(gcmEncrypt(aes,iv,toBytes(data),aad,tagBytes).buffer); return;
          } catch (e) { reject(e); }
        });
      },
      decrypt: function (algorithm, key, data) {
        return new Promise(function (resolve, reject) {
          try {
            var alg=String((algorithm&&algorithm.name)||'').toUpperCase();
            if(alg!=='AES-GCM'){ reject(new Error('Unsupported decrypt algorithm: '+alg)); return; }
            var aes=new AES(key.key), iv=toBytes(algorithm.iv);
            var aad=algorithm.additionalData?toBytes(algorithm.additionalData):new Uint8Array(0);
            var tagBytes=(algorithm.tagLength!=null?algorithm.tagLength:128)/8;
            var all=toBytes(data);
            if(all.length<tagBytes){ reject(new Error('AES-GCM: ciphertext too short')); return; }
            var C=all.slice(0,all.length-tagBytes), T=all.slice(all.length-tagBytes);
            var pt=gcmDecrypt(aes,iv,C,aad,T,tagBytes);
            if(pt===null){ reject(new Error('AES-GCM: authentication failed')); return; }
            resolve(pt.buffer); return;
          } catch (e) { reject(e); }
        });
      }
    };
    // ---- CSPRNG: HMAC-DRBG (SHA-256) seeded with OS entropy injected by the host ----
    function __sthHexToBytes(h){var o=new Uint8Array(h.length/2);for(var i=0;i<o.length;i++)o[i]=parseInt(h.substr(i*2,2),16);return o;}
    var __sthDrbg=(function(){
      var K=new Uint8Array(32), V=new Uint8Array(32); V.fill(1);
      function upd(prov){
        K=hmacBytes('SHA-256',K,concatBytes(concatBytes(V,new Uint8Array([0])),prov||new Uint8Array(0))); V=hmacBytes('SHA-256',K,V);
        if(prov&&prov.length){ K=hmacBytes('SHA-256',K,concatBytes(concatBytes(V,new Uint8Array([1])),prov)); V=hmacBytes('SHA-256',K,V); }
      }
      var seedHex='__STH_CSPRNG_SEED_HEX__';
      var seed = /^[0-9a-fA-F]{32,}$/.test(seedHex) ? __sthHexToBytes(seedHex) : null;
      if(!seed){ seed=new Uint8Array(48); for(var i=0;i<48;i++) seed[i]=Math.floor(Math.random()*256);
        var now=Date.now(), t=new Uint8Array(8); for(var j=0;j<8;j++){t[j]=now&0xff;now=Math.floor(now/256);} seed=concatBytes(seed,t); }
      upd(seed);
      return { gen:function(n){ var out=new Uint8Array(n), off=0; while(off<n){ V=hmacBytes('SHA-256',K,V); var c=Math.min(32,n-off); out.set(V.subarray(0,c),off); off+=c; } upd(null); return out; } };
    })();
    if (typeof C.getRandomValues !== 'function') {
      C.getRandomValues = function (a) { var b=new Uint8Array(a.buffer,a.byteOffset,a.byteLength); b.set(__sthDrbg.gen(b.length)); return a; };
    }
    if (typeof C.randomUUID !== 'function') {
      C.randomUUID = function () {
        var b=new Uint8Array(16); C.getRandomValues(b); b[6]=(b[6]&0x0f)|0x40; b[8]=(b[8]&0x3f)|0x80;
        var h=[]; for(var i=0;i<16;i++) h.push((b[i]+0x100).toString(16).slice(1));
        return h.slice(0,4).join('')+'-'+h.slice(4,6).join('')+'-'+h.slice(6,8).join('')+'-'+h.slice(8,10).join('')+'-'+h.slice(10,16).join('');
      };
    }
    try { Object.defineProperty(C, 'subtle', { value: subtle, configurable: true }); }
    catch (e) { try { C.subtle = subtle; } catch (e2) {} }
    try { Object.defineProperty(g, 'isSecureContext', { value: true, configurable: true }); } catch (e) {}
    try { Object.defineProperty(g, '__sthCryptoPolyfilled', { value: true, configurable: true }); } catch (e) { try { g.__sthCryptoPolyfilled = true; } catch (e2) {} }
  } catch (e) { /* never break the dApp */ }
})();
"#;

/// Build a per-webview crypto polyfill script with a fresh 64-byte OS-CSPRNG seed
/// embedded for the in-page HMAC-DRBG (so dApp `getRandomValues`/key-gen is
/// cryptographically strong instead of falling back to `Math.random`).
pub(crate) fn crypto_polyfill_script() -> String {
    use rand::RngCore;
    let mut seed = [0u8; 64];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let hex: String = seed.iter().map(|b| format!("{:02x}", b)).collect();
    CRYPTO_POLYFILL_JS.replace("__STH_CSPRNG_SEED_HEX__", &hex)
}

pub(crate) const DAPP_BRIDGE_JS: &str = r#"
(function () {
  if (window.smartholdem) return;
  var pending = {};
  function T() { return window.__TAURI__; }
  // dApps load at sth://<APP_ID>/ — the id is the HOST (authority). The host is
  // injected by the Rust webview builder (window.__STH_APP_ID__) because the
  // raw hostname is platform-dependent (Windows maps it to <id>.sth.localhost).
  // Fall back to normalizing the live hostname if the injection is absent.
  var APP_ID = window.__STH_APP_ID__ || (function () {
    var h = (location.hostname || '').replace(/\.localhost$/i, '').replace(/\.sth$/i, '');
    if (h && h.toLowerCase() !== 'sth' && h.toLowerCase() !== 'localhost') return h;
    return ((location.pathname || '/').replace(/^\/+/, '').split('/')[0]) || '';
  })();
  function emit(name, payload) { try { if (T() && T().event) T().event.emit(name, payload); } catch (e) {} }
  // Stream a runtime log line to the host shell terminal (and the dev console).
  function tlog(text, type) {
    var t = type || 'info';
    try {
      var col = t === 'success' ? 'color:#34d399' : t === 'error' ? 'color:#fb7185'
        : t === 'network' ? 'color:#fbbf24' : 'color:#22d3ee';
      console.log('%c' + text, col);
    } catch (e) {}
    emit('dapp-terminal-log', { app_id: APP_ID, text: text, type: t });
  }
  function ensureListener() {
    if (window.__sthBridgeReady || !T() || !T().event) return;
    window.__sthBridgeReady = true;
    T().event.listen('dapp-response', function (e) {
      var m = (e && e.payload) || {};
      var p = pending[m.id];
      if (!p) return;
      delete pending[m.id];
      if (m.error) { tlog('[sth://network] ✗ ' + m.error, 'error'); p.reject(new Error(m.error)); }
      else { tlog('[sth://network] ✓ response received', 'network'); p.resolve(m.result); }
    });
  }
  // Overlay ожидания подтверждения удалён — WalletDrawer в главном окне
  // раскрывается автоматически и уже даёт весь UX (Approve/Reject).
  // Никаких доп. модалок ни внутри DApp-webview, ни поверх главного окна.
  function request(method, params) {
    return new Promise(function (resolve, reject) {
      if (!T() || !T().event) { reject(new Error('smartholdem: bridge unavailable')); return; }
      ensureListener();
      var id = Date.now().toString(36) + Math.random().toString(36).slice(2);
      pending[id] = { resolve: resolve, reject: reject };
      var origin = location.origin && location.origin !== 'null' ? location.origin : location.href;
      tlog('[sth://network] → ' + method, 'network');
      Promise.resolve(T().event.emit('dapp-request', { id: id, method: method, params: params || {}, origin: origin }))
        .catch(function (err) { delete pending[id]; reject(new Error(String(err))); });
      setTimeout(function () {
        if (pending[id]) { delete pending[id]; reject(new Error('smartholdem: request timeout')); }
      }, 120000);
    });
  }
  window.smartholdem = {
    isSmartHoldem: true,
    version: '1.0.0',
    network: 'mainnet',
    getAccount: function () { return request('getAccount'); },
    signMessage: function (message) { return request('signMessage', { message: message }); },
    signTransaction: function (payload) { return request('signTransaction', payload); },
    sendTransaction: function (payload) { return request('sendTransaction', payload); },
    requestSwap: function (payload) { return request('requestSwap', payload); },
    // Раскрыть панель кошелька в host-shell (для пункта меню dApp'а
    // «Wallet»). Ничего не подписывает и не требует подтверждения.
    openWallet: function () { return request('openWallet'); },
    // ---- Netfory AI Compute: useAI({prompt, model?, maxTokens?, onToken?}) --
    //   Стриминговая генерация на децентрализованном AI-суперкомпьютере.
    //   Возвращает { streamId, done: Promise<{text,node,ms}>, cancel() }.
    //   onToken(t) вызывается на каждый новый кусок текста.
    useAI: function (opts) {
      opts = opts || {};
      var sid = 'ai' + Date.now().toString(36) + Math.random().toString(36).slice(2);
      var api = T();
      var unToken = null;
      function cleanup() { if (unToken) { try { unToken(); } catch (e) {} unToken = null; } }
      var done = new Promise(function (resolve, reject) {
        if (!api || !api.event) { reject(new Error('smartholdem: bridge unavailable')); return; }
        ensureListener();
        api.event.listen('dapp-ai-token', function (e) {
          var m = (e && e.payload) || {};
          if (m.streamId !== sid) return;
          try { if (opts.onToken) opts.onToken(m.t || ''); } catch (err) {}
        }).then(function (un) { unToken = un; });
        pending[sid] = {
          resolve: function (r) { cleanup(); tlog('[sth://ai] ✓ done (' + ((r && r.ms) || 0) + ' ms)', 'success'); resolve(r); },
          reject: function (e) { cleanup(); reject(e instanceof Error ? e : new Error(String(e))); }
        };
        tlog('[sth://ai] → generate (' + String(opts.prompt || '').length + ' chars' + (opts.model ? ', ' + opts.model : '') + ')', 'network');
        Promise.resolve(api.event.emit('dapp-request', {
          id: sid,
          method: 'aiGenerate',
          params: {
            streamId: sid,
            prompt: String(opts.prompt || ''),
            model: opts.model || null,
            maxTokens: opts.maxTokens || 256
          },
          origin: (location.origin && location.origin !== 'null') ? location.origin : location.href
        })).catch(function (err) { var p = pending[sid]; delete pending[sid]; if (p) p.reject(new Error(String(err))); });
        // генерация может быть долгой — таймаут 10 минут
        setTimeout(function () {
          if (pending[sid]) { var p = pending[sid]; delete pending[sid]; p.reject(new Error('useAI: timeout')); }
        }, 600000);
      });
      return {
        streamId: sid,
        done: done,
        cancel: function () {
          emit('dapp-request', {
            id: sid + '-cancel',
            method: 'aiCancel',
            params: { streamId: sid },
            origin: (location.origin && location.origin !== 'null') ? location.origin : location.href
          });
        }
      };
    }
  };

  // ---- ClearNet egress broker (v1.63.7) ------------------------------
  //   У embed-*/app-* webview'ов НЕТ разрешения shell:allow-open (ACL),
  //   поэтому попытка dApp открыть внешнюю ссылку (window.open,
  //   __TAURI__.shell.open, <a href="https://...">) падала с
  //   "shell.open not allowed". Перехватываем все три пути и пересылаем
  //   URL host-shell'у событием 'dapp-open-url' — там он открывается во
  //   web-вкладке NETFORY (v1.135.225: без белого списка и предупреждений).
  function isExternalHttp(href) {
    if (!/^https?:\/\//i.test(href)) return false;
    try {
      var h = new URL(href).hostname.toLowerCase();
      return h !== (location.hostname || '').toLowerCase() && h !== 'localhost' && !/\.localhost$/.test(h);
    } catch (e) { return false; }
  }
  function forwardExternal(href) {
    tlog('[clearnet] \u2192 host: ' + String(href).slice(0, 120), 'network');
    emit('dapp-open-url', { app_id: APP_ID, url: String(href || '') });
  }
  // ---- Cross-dApp linking (v1.63.11) ---------------------------------
  //   Кастомные схемы SmartNet (sth:// u:// sn:// api:// f:// dev://) в
  //   href / window.open должны открываться НОВОЙ ВКЛАДКОЙ в host-shell,
  //   а не навигацией внутри webview. Клики и window.open перехватываем
  //   здесь; программный window.location.href добивает Rust-хук
  //   on_navigation (объект location из JS переопределить нельзя).
  function isHostLink(href) {
    var m = /^(sth|u|sn|api|f|dev):\/\//i.exec(String(href || ''));
    if (!m) return false;
    if (m[1].toLowerCase() === 'sth') {
      // sth:// на СВОЙ ЖЕ appId — внутренняя SPA-навигация, не трогаем.
      var rest = String(href).slice(6);
      var cut = rest.search(/[\/?#]/);
      var authority = cut === -1 ? rest : rest.slice(0, cut);
      if (authority === APP_ID) return false;
    }
    return true;
  }
  function forwardHostLink(href) {
    tlog('[link] \u2192 host tab: ' + String(href).slice(0, 120), 'network');
    emit('dapp-open-link', { app_id: APP_ID, url: String(href || '') });
  }
  // ---- Internal target="_blank" (v1.89.4) ----------------------------
  //   Если dApp кликает <a target="_blank"> на СВОЮ ЖЕ страницу
  //   (относительный путь либо тот же sth://APP_ID/...), webview блокирует
  //   создание новой вкладки (нет permission на window.open внешне,
  //   а <a target=_blank> не проходит через нашу подмену window.open).
  //   Резолвим href относительно текущего location в абсолютный
  //   sth://APP_ID/<path> и просим host-shell открыть НОВОЕ окно.
  function isInternalOrSameApp(href) {
    if (!href) return false;
    var s = String(href);
    if (isExternalHttp(s)) return false;
    // sth:// на СВОЙ appId — считается «своим»
    var m = /^sth:\/\/([^\/?#]+)/i.exec(s);
    if (m) return m[1].toLowerCase() === String(APP_ID).toLowerCase();
    // Явно чужие схемы (host-link) — не наш случай
    if (/^(u|sn|api|f|dev|mailto|tel):/i.test(s)) return false;
    // Всё остальное (относительный путь, /path, ?q, #frag) — внутреннее
    return true;
  }
  function toAbsoluteSthUrl(href) {
    try {
      var abs = new URL(String(href), location.href).href;
      // Убеждаемся, что осталось sth://<APP_ID>/…
      if (/^sth:\/\//i.test(abs)) return abs;
      // Если URL распарсился в http(s)://<app_id>.sth.localhost/... (Windows),
      // всё равно строим канонический sth://APP_ID/<pathname+search+hash>.
      var u = new URL(abs);
      return 'sth://' + APP_ID + u.pathname + u.search + u.hash;
    } catch (e) {
      return 'sth://' + APP_ID + '/' + String(href).replace(/^\/+/, '');
    }
  }
  function forwardNewWindow(href) {
    var abs = toAbsoluteSthUrl(href);
    tlog('[link] \u2192 new window: ' + abs.slice(0, 120), 'network');
    emit('dapp-open-link', { app_id: APP_ID, url: abs, new_window: true });
  }
  var __origOpen = window.open ? window.open.bind(window) : null;
  window.open = function (u, t, f) {
    var href = typeof u === 'string' ? u : (u && u.toString ? u.toString() : '');
    if (isExternalHttp(href)) { forwardExternal(href); return null; }
    if (isHostLink(href)) { forwardHostLink(href); return null; }
    // window.open('/path') или window.open('/path','_blank') — своё окно
    if (isInternalOrSameApp(href) && String(t || '_blank') !== '_self') {
      forwardNewWindow(href);
      return null;
    }
    return __origOpen ? __origOpen(u, t, f) : null;
  };
  function patchShellOpen() {
    try {
      var api = T();
      if (api && api.shell && !api.shell.__sthEgress) {
        api.shell.__sthEgress = true;
        api.shell.open = function (href) { forwardExternal(href); return Promise.resolve(); };
      }
      if (api && api.opener && !api.opener.__sthEgress) {
        api.opener.__sthEgress = true;
        if (api.opener.openUrl) api.opener.openUrl = function (href) { forwardExternal(href); return Promise.resolve(); };
      }
    } catch (e) {}
  }
  patchShellOpen();
  document.addEventListener('DOMContentLoaded', patchShellOpen);
  setTimeout(patchShellOpen, 1500);
  document.addEventListener('click', function (e) {
    var t = e.target;
    var a = t && t.closest ? t.closest('a[href]') : null;
    if (!a) return;
    var href = a.getAttribute('href') || '';
    if (isExternalHttp(href)) {
      e.preventDefault();
      e.stopPropagation();
      forwardExternal(href);
      return;
    }
    if (isHostLink(href)) {
      e.preventDefault();
      e.stopPropagation();
      forwardHostLink(href);
    }
  }, true);

  // ---- Runtime detection (`window.__SMARTNET__`) --------------------
  //   Стабильный, синхронный сниф-объект: dApp может прочитать его ДО
  //   первого fetch/mount, чтобы решить, какой backend использовать
  //   (api:// P2P против clearnet https:// fallback). Значения ставит
  //   Rust на этапе initialization_script — приезжают ДО загрузки любого
  //   JS страницы, поэтому Vue/Vite увидит их синхронно на старте.
  //   ВАЖНО: не заменяем поля, если dApp уже что-то положил в __SMARTNET__
  //   (для тестов/локального dev-режима). Используем Object.freeze, чтобы
  //   dApp не мог случайно перезаписать `protocol: 'api'` → 'https'.
  try {
    if (!window.__SMARTNET__) {
      Object.defineProperty(window, '__SMARTNET__', {
        value: Object.freeze({
          isSmartNet: true,             // главный флаг для условий
          protocol: 'api',              // рекомендованный транспорт
          appId: APP_ID,                // сам dApp (sth://<id>)
          clientVersion: '__STH_CLIENT_VERSION__', // подставляется в Rust
          network: 'mainnet',
          features: Object.freeze({
            wallet: true,
            apiScheme: true,             // fetch('api://...') работает
            fileScheme: true,            // fetch('f://<cid>/...') работает
            translate: true,             // window.__sthI18n
            crypto: true,                // window.crypto.subtle (polyfilled)
            ai: true                     // window.smartholdem.useAI()
          })
        }),
        writable: false,
        configurable: false,
        enumerable: true
      });
    }
  } catch (e) {}

  // ---- Network tap: наблюдаем network-запросы dApp'а для SystemConsole.
  //   Мы НЕ проксируем и НЕ модифицируем — WebView выполняет fetch/XHR сам,
  //   мы только измеряем время, снимаем заголовки/тело и эмитим событие
  //   `dapp-log-event` в Rust, который дублирует запись в глобальную
  //   SystemConsole со scope='dapp'. Детальный payload — headers / body
  //   preview / content-length — проксируется как `dapp-log-detail` для
  //   отображения в popup.
  //   Отключить нельзя (audit-log by design) — но и информации ноль сверх
  //   того, что уже видит сам браузер.
  (function tapNetwork() {
    if (window.__sthNetTapped) return;
    window.__sthNetTapped = true;
    var api = T();
    var origin = (window.smartholdem && document.location && document.location.host) || '';
    function shortId() { return Math.random().toString(36).slice(2, 10); }
    // Skip Tauri v2 internal IPC — otherwise every `api.event.emit()` inside
    // this very tap (`dapp-log-event`) triggers a fetch to `ipc.localhost`
    // that gets logged, which triggers another emit, etc. → recursive spam
    // that instantly fills the 500-entry ring buffer. WebView2/WKWebView
    // route IPC through `http://ipc.localhost/*` (Windows/Linux) and
    // `https://tauri.localhost/*` (macOS custom scheme fallback).
    function isInternalIpc(u) {
      if (!u) return true;
      var s = String(u).toLowerCase();
      // substring-match (не prefix), потому что fetch может нормализовать
      // URL с query-параметрами / относительным путём. Всё что содержит
      // `ipc.localhost` или `tauri.localhost` — это Tauri IPC, а не сеть.
      return s.indexOf('ipc.localhost') !== -1
          || s.indexOf('tauri.localhost') !== -1
          || s.indexOf('tauri://') === 0
          || s.indexOf('ipc://') === 0
          || s.indexOf('asset://') === 0
          || s.indexOf('blob:') === 0
          || s.indexOf('data:') === 0
          || s.indexOf('/plugin:') !== -1
          || s.indexOf('plugin%3a') !== -1;
    }
    function emit(rec) {
      if (isInternalIpc(rec.url)) return;
      // Anti-recursion guard: если tap уже эмитит — не даём хуку XHR/fetch
      // самому вызвать себя (некоторые webview'ы синхронно pump'ят fetch).
      if (window.__sthEmittingLog) return;
      window.__sthEmittingLog = true;
      rec.origin = origin;
      try { if (api && api.event) api.event.emit('dapp-log-event', rec); } catch (e) {}
      window.__sthEmittingLog = false;
    }
    // Serialize a headers-like object into a plain string:string map.
    function serializeHeaders(h) {
      var out = {};
      if (!h) return out;
      try {
        if (typeof h.forEach === 'function' && !Array.isArray(h)) { h.forEach(function (v, k) { out[k] = String(v); }); }
        else if (Array.isArray(h)) { for (var i = 0; i < h.length; i++) out[h[i][0]] = String(h[i][1]); }
        else if (typeof h === 'object') { for (var k in h) if (Object.prototype.hasOwnProperty.call(h, k)) out[k] = String(h[k]); }
      } catch (e) {}
      return out;
    }
    // Snapshot body → string preview (max 2 KiB, avoid huge memory).
    function bodyPreview(body) {
      if (body == null) return '';
      try {
        if (typeof body === 'string') return body.slice(0, 2048);
        if (typeof URLSearchParams !== 'undefined' && body instanceof URLSearchParams) return body.toString().slice(0, 2048);
        if (typeof FormData !== 'undefined' && body instanceof FormData) return '[FormData]';
        if (body instanceof Blob) return '[Blob ' + body.size + ' bytes]';
        if (body instanceof ArrayBuffer) return '[ArrayBuffer ' + body.byteLength + ' bytes]';
        return JSON.stringify(body).slice(0, 2048);
      } catch (e) { return '[unserializable]'; }
    }
    // fetch tap
    if (window.fetch) {
      var nativeFetch = window.fetch.bind(window);
      // api:// polyfill: WebkitGTK/WKWebView блокируют cross-scheme fetch
      // (sth:// → api://) по CORS ЕЩЁ до нашего uri-scheme handler'а. Ловим
      // такие URL'ы прямо в JS и роутим через bridge (`window.smartholdem`
      // → dapp-request → Rust `apiFetch` → p2p::api_request). Синтезируем
      // стандартный `Response` из результата — dApp не отличит от обычного
      // HTTP: `.status`, `.ok`, `.headers.get(...)`, `.json()`, `.text()`.
      function apiFetchViaBridge(url, method, bodyStr, reqHeaders, t0, tapId) {
        return new Promise(function (resolve, reject) {
          var api = T();
          if (!api || !api.event) { reject(new TypeError('smartholdem: bridge unavailable for ' + url)); return; }
          ensureListener();
          var id = Date.now().toString(36) + Math.random().toString(36).slice(2);
          pending[id] = {
            resolve: function (r) {
              // r = { status, body, contentType, sigVerified, provider, signedAt, relayed, target }
              var status = (r && r.status) || 200;
              var body = (r && typeof r.body === 'string') ? r.body : '';
              var ct = (r && r.contentType) || 'application/json; charset=utf-8';
              var headers = new Headers({
                'Content-Type': ct,
                'X-Sth-Sig-Verified': (r && r.sigVerified) ? '1' : '0',
                'X-Sth-Provider': (r && r.provider) || '',
                'X-Sth-Signed-At': String((r && r.signedAt) || 0),
                'X-Sth-Relayed': (r && r.relayed) ? '1' : '0',
                'X-Sth-Target': (r && r.target) || ''
              });
              var resp = new Response(body, { status: status, statusText: '', headers: headers });
              // Логируем в SystemConsole (scope=dapp) как обычный fetch.
              emit({
                id: tapId, method: method, url: url, status: status,
                ms: Date.now() - t0, ok: status >= 200 && status < 300,
                size: body.length, reqHeaders: reqHeaders, reqBody: bodyStr,
                respHeaders: { 'content-type': ct, 'x-sth-sig-verified': headers.get('X-Sth-Sig-Verified') || '0' },
                contentType: ct
              });
              resolve(resp);
            },
            reject: function (e) {
              emit({
                id: tapId, method: method, url: url, status: 0,
                ms: Date.now() - t0, ok: false, err: String(e && e.message || e),
                reqHeaders: reqHeaders, reqBody: bodyStr
              });
              reject(new TypeError('Failed to fetch (api://): ' + (e && e.message || e)));
            }
          };
          Promise.resolve(api.event.emit('dapp-request', {
            id: id,
            method: 'apiFetch',
            params: { url: url, method: method, body: bodyStr || '', headers: reqHeaders || {} },
            origin: (location.origin && location.origin !== 'null') ? location.origin : location.href
          })).catch(function (e) { delete pending[id]; pending[id] && pending[id].reject(e); });
          // Таймаут — тот же 120s что и у обычного bridge request.
          setTimeout(function () {
            if (pending[id]) { var p = pending[id]; delete pending[id]; p.reject(new Error('api:// request timeout')); }
          }, 120000);
        });
      }
      window.fetch = function (input, init) {
        var t0 = Date.now();
        var opts = init || (typeof input === 'object' && input) || {};
        var method = String(opts.method || 'GET').toUpperCase();
        var url = typeof input === 'string' ? input : (input && input.url) || '';
        var reqHeaders = serializeHeaders(opts.headers);
        var reqBody = bodyPreview(opts.body);
        var id = shortId();
        // Перехват api:// — идём через bridge, обходим CORS/scheme-блок WebView.
        if (/^api:\/\//i.test(url)) {
          // Трекаем nodeId → provider: WS-полифилл использует эту мапу, потому
          // что socket.io-client теряет имя провайдера при построении WS URL
          // (он берёт из BASE только host, path берёт из opts). Так dApp
          // может использовать `io(BASE)` без изменений — мы восстановим
          // /pokersth автоматически.
          try {
            var m = /^api:\/\/([a-f0-9]{64})\/([^/?#]+)/i.exec(url);
            if (m) {
              window.__STH_PROVIDER_BY_NODE__ = window.__STH_PROVIDER_BY_NODE__ || {};
              window.__STH_PROVIDER_BY_NODE__[m[1].toLowerCase()] = m[2];
            }
          } catch (e) {}
          return apiFetchViaBridge(url, method, reqBody, reqHeaders, t0, id);
        }
        return nativeFetch(input, init).then(function (resp) {
          var respHeaders = {};
          try { resp.headers.forEach(function (v, k) { respHeaders[k] = v; }); } catch (e) {}
          var size = 0;
          try { size = parseInt(respHeaders['content-length'] || '0', 10) || 0; } catch (e) {}
          emit({
            id: id, method: method, url: url, status: resp.status,
            ms: Date.now() - t0, ok: resp.ok, size: size,
            reqHeaders: reqHeaders, reqBody: reqBody,
            respHeaders: respHeaders, contentType: respHeaders['content-type'] || ''
          });
          return resp;
        }, function (err) {
          emit({
            id: id, method: method, url: url, status: 0,
            ms: Date.now() - t0, ok: false,
            err: String(err && err.message || err),
            reqHeaders: reqHeaders, reqBody: reqBody
          });
          throw err;
        });
      };
    }
    // XHR tap (headers доступны только частично, тело pre-send — сериализуем)
    if (window.XMLHttpRequest) {
      var XHR = window.XMLHttpRequest.prototype;
      var origOpen = XHR.open, origSend = XHR.send, origSetHeader = XHR.setRequestHeader;
      XHR.open = function (m, u) {
        this.__sthM = String(m || 'GET').toUpperCase();
        this.__sthU = String(u || '');
        this.__sthReqH = {};
        this.__sthId = shortId();
        return origOpen.apply(this, arguments);
      };
      XHR.setRequestHeader = function (k, v) { if (this.__sthReqH) this.__sthReqH[k] = String(v); return origSetHeader.apply(this, arguments); };
      XHR.send = function (b) {
        var self = this, t0 = Date.now();
        var reqBody = bodyPreview(b);
        function done() {
          var respHeaders = {};
          try {
            var raw = self.getAllResponseHeaders() || '';
            raw.split(/\r?\n/).forEach(function (line) {
              var i = line.indexOf(':');
              if (i > 0) respHeaders[line.slice(0, i).trim().toLowerCase()] = line.slice(i + 1).trim();
            });
          } catch (e) {}
          var size = 0;
          try { size = parseInt(respHeaders['content-length'] || '0', 10) || 0; } catch (e) {}
          emit({
            id: self.__sthId, method: self.__sthM || 'GET', url: self.__sthU || '',
            status: self.status || 0, ms: Date.now() - t0,
            ok: self.status >= 200 && self.status < 300, size: size,
            reqHeaders: self.__sthReqH || {}, reqBody: reqBody,
            respHeaders: respHeaders, contentType: respHeaders['content-type'] || ''
          });
        }
        this.addEventListener('loadend', done);
        return origSend.apply(this, arguments);
      };
    }

    // ---- WebSocket-полифилл для ws(s)://<hexNodeId>/… ------------------
    //  Ловим только когда host — 64-hex Ed25519 NodeID. Обычные wss://
    //  сайты идут через нативный WebSocket без вмешательства.
    if (window.WebSocket) {
      var NativeWS = window.WebSocket;
      var NODE_RE = /^wss?:\/\/[a-f0-9]{64}\//i;
      // v1.55.36: WS через событийный bridge (dapp-request/dapp-response) вместо
      // invoke. Tauri ACL ВСЕГДА запрещает команды приложения для remote-origin
      // webview'ов («Command dapp_ws_open not allowed by ACL») — capability
      // dapp-bridge даёт remote-страницам только core:event:default. Поэтому
      // wsOpen/wsSend/wsClose идут тем же каналом, что и apiFetch.
      function wsBridge(method, params) {
        return new Promise(function (resolve, reject) {
          var api = T();
          if (!api || !api.event) { reject(new Error('smartholdem: bridge unavailable')); return; }
          ensureListener();
          var id = Date.now().toString(36) + Math.random().toString(36).slice(2);
          pending[id] = { resolve: resolve, reject: reject };
          Promise.resolve(api.event.emit('dapp-request', {
            id: id, method: method, params: params || {},
            origin: (location.origin && location.origin !== 'null') ? location.origin : location.href
          })).catch(function (e) { delete pending[id]; reject(new Error(String(e))); });
          setTimeout(function () {
            if (pending[id]) { delete pending[id]; reject(new Error(method + ': bridge timeout')); }
          }, 120000);
        });
      }
      // Нормализация URL: socket.io-client / чистый WebSocket API строят WS URL
      // как ws://<nodeId>/<path> без имени провайдера (он не знает наш формат
      // api://<nodeId>/<provider>/<path>). Пытаемся восстановить его двумя
      // способами:
      //   1) window.__STH_PROVIDER_BY_NODE__[<hexNodeId>] — заполняется
      //      автоматически при api-fetch (см. fetch tap выше).
      //   2) window.__SMARTNET__.provider — можно явно выставить в dApp'е.
      // Если после этого путь всё равно начинается с уже-нашего provider —
      // не дублируем. Возвращаем `null` если провайдер не удалось определить
      // — тогда StubWs эмитит close(1006, 'unknown provider…').
      function resolveWsUrl(url) {
        var m = /^(wss?):\/\/([a-f0-9]{64})(\/[^?#]*)?(\?[^#]*)?/i.exec(url);
        if (!m) return null;
        var scheme = m[1], node = m[2].toLowerCase();
        var pathPart = m[3] || '/';
        var qs = m[4] || '';
        var map = window.__STH_PROVIDER_BY_NODE__ || {};
        var known = map[node] || (window.__SMARTNET__ && window.__SMARTNET__.provider);
        // v1.55.34: fallback — провайдер неизвестен (WS открыт ДО первого
        // api:// fetch), но путь уже содержит сегменты → пропускаем URL как
        // есть: Rust трактует первый сегмент пути как имя провайдера
        // (ws://<node>/pokersth/api/ws/…). Ошибка роутинга, если сегмент не
        // провайдер, будет видна в консоли — лучше, чем немое close(1006).
        if (!known) {
          if (pathPart !== '/' && pathPart.length > 1) {
            return scheme + '://' + node + pathPart + qs;
          }
          return null;
        }
        // Если path уже начинается с /<known>/ — не дублируем.
        var pre = '/' + known;
        if (pathPart === pre || pathPart.indexOf(pre + '/') === 0) {
          return scheme + '://' + node + pathPart + qs;
        }
        // Иначе вставляем provider первым сегментом path.
        return scheme + '://' + node + pre + pathPart + qs;
      }
      // v1.55.35: «спасение» WS-URL c потерянным nodeId. socket.io-client не
      // понимает схему api:// — при io('api://<node>/<provider>') парсер
      // теряет authority и строит ws://api/<provider>/… (host='api').
      // Восстанавливаем nodeId по карте __STH_PROVIDER_BY_NODE__ (заполняется
      // при api:// fetch): если первый сегмент пути совпадает с известным
      // провайдером — берём его node; иначе, если известен ровно один node,
      // берём его. Спасаем только хосты без точки ('api', 'localhost'-подобные)
      // — реальные внешние wss://host.tld идут в нативный WebSocket.
      function rescueWsUrl(url) {
        var m = /^(wss?):\/\/([^/?#]*)(\/[^?#]*)?(\?[^#]*)?/i.exec(url);
        if (!m) return null;
        var host = m[2] || '';
        if (host.indexOf('.') !== -1 || host.indexOf(':') !== -1) return null;
        var pathPart = m[3] || '/';
        var qs = m[4] || '';
        var map = window.__STH_PROVIDER_BY_NODE__ || {};
        var nodes = Object.keys(map);
        if (!nodes.length) return null;
        var firstSeg = (pathPart.split('/')[1] || '').toLowerCase();
        var node = null;
        for (var i = 0; i < nodes.length; i++) {
          if (String(map[nodes[i]]).toLowerCase() === firstSeg) { node = nodes[i]; break; }
        }
        if (!node && nodes.length === 1) node = nodes[0];
        if (!node) return null;
        var prov = map[node];
        var pre = '/' + prov;
        var full = (pathPart === pre || pathPart.indexOf(pre + '/') === 0) ? pathPart : pre + pathPart;
        return m[1] + '://' + node + full + qs;
      }
      function StubWs(url, protocols) {
        if (!NODE_RE.test(url)) {
          var rescued = rescueWsUrl(url);
          if (rescued) {
            tlog('[sth://ws] rescue ' + url + ' \u2192 ' + rescued, 'network');
            url = rescued;
          } else {
            tlog('[sth://ws] passthrough native WS: ' + url, 'network');
            return new NativeWS(url, protocols);
          }
        }
        var normalized = resolveWsUrl(url);
        tlog('[sth://ws] intercept ' + url + (normalized ? (' \u2192 ' + normalized) : ' \u2192 provider \u043d\u0435 \u043e\u043f\u0440\u0435\u0434\u0435\u043b\u0451\u043d'), 'network');
        var self = this;
        self.url = url;
        self.readyState = 0;
        self.protocol = '';
        self.extensions = '';
        self.bufferedAmount = 0;
        self.binaryType = 'blob';
        self.onopen = null; self.onmessage = null; self.onclose = null; self.onerror = null;
        self._listeners = { open: [], message: [], close: [], error: [] };
        self._streamId = null;

        var api = T();
        if (!api || !api.event) {
          setTimeout(function () { self._fireClose(1006, 'tauri bridge unavailable'); }, 0);
          return;
        }
        if (!normalized) {
          setTimeout(function () {
            tlog('[sth://ws] unknown provider \u0434\u043b\u044f nodeId \u2014 \u0441\u0434\u0435\u043b\u0430\u0439\u0442\u0435 api:// fetch \u0434\u043e WS \u0438\u043b\u0438 \u0437\u0430\u0434\u0430\u0439\u0442\u0435 window.__SMARTNET__.provider', 'error');
            self._fire('error', { type: 'error', message: 'unknown provider — сделайте хотя бы один api:// fetch до открытия WS или задайте window.__SMARTNET__.provider' });
            self._fireClose(1006, 'unknown provider for nodeId');
          }, 0);
          return;
        }

        // Sec-WebSocket-Protocol из protocols argument. Спека:
        // если protocols — string, используем как один subprotocol;
        // если Array — join(', ') (RFC 6455 §4.1).
        var reqHeaders = {};
        if (protocols) {
          var subp = Array.isArray(protocols) ? protocols.join(', ') : String(protocols);
          if (subp) reqHeaders['sec-websocket-protocol'] = subp;
        }

        var unlistenMsg = null, unlistenClose = null;
        // v1.55.32: РАДИКАЛЬНЫЙ фикс WS race condition в DApp-полифилле.
        //
        // РАНЬШЕ: listener'ы 'dapp-ws-*' регистрировались ВНУТРИ .then() после
        // resolve invoke — из-за этого:
        //   1) Событие 'dapp-ws-open' эмитилось Rust'ом до resolve → терялось.
        //   2) Первые message-фреймы (напр. Socket.IO OPEN packet) успевали
        //      прилететь ДО регистрации listener'а → терялись.
        //   → poker Socket.IO падал на «websocket error», readyState=0 навсегда.
        //
        // ТЕПЕРЬ: (а) listener'ы регистрируются ДО invoke; (б) до resolve invoke
        // streamId неизвестен → буферизуем ВСЕ входящие события, (в) после
        // resolve выбираем из буфера события с нашим streamId и доставляем.
        // (г) `status` теперь возвращается из самого invoke Promise (Rust
        // возвращает `{stream_id, status}` синхронно после handshake ACK) —
        // событие 'dapp-ws-open' больше не источник истины.
        var pendingEvents = [];
        var mySid = null;
        // v1.55.37: детектор socket.io CONNECT_ERROR «Invalid namespace».
        // socket.io-client интерпретирует pathname URL как namespace, поэтому
        // io('sth://<node>/<provider>') → namespace=/<provider>, а сервер, у
        // которого зарегистрирован только root namespace, отвечает пакетом
        // engine.io MESSAGE (opcode '4') + socket.io CONNECT_ERROR (opcode '4')
        // → «44{"message":"Invalid namespace"}». Ловим первый такой ответ и
        // печатаем в System Console готовую подсказку с каноничным вызовом io().
        function maybePrintNsHint(text) {
          if (self._nsHintShown) return;
          if (typeof text !== 'string' || text.length < 2) return;
          // Быстрая отсечка: интересует только engine.io MESSAGE (4) +
          // socket.io CONNECT_ERROR (4) — оба символа '4' (charCode 52).
          if (text.charCodeAt(0) !== 52 || text.charCodeAt(1) !== 52) return;
          if (text.indexOf('Invalid namespace') === -1) return;
          self._nsHintShown = true;
          var nm = /^wss?:\/\/([a-f0-9]{64})\/([^/?#]+)(\/[^?#]*)?/i.exec(normalized || '');
          var node = nm ? nm[1] : '<nodeId>';
          var prov = nm ? nm[2] : '<provider>';
          var rest = nm && nm[3] ? nm[3] : '/socket.io/';
          // Собираем path так, чтобы provider был первым сегментом; если DApp
          // уже указал /socket.io/ — просто добавляем /<provider> перед ним.
          var pathHint = '/' + prov + (rest.indexOf('/socket.io') === 0 ? rest : '/socket.io/');
          var hint =
            '[sth://ws] socket.io \u043e\u0442\u0432\u0435\u0442\u0438\u043b \u00abInvalid namespace\u00bb. ' +
            'DApp \u043f\u0435\u0440\u0435\u0434\u0430\u043b \u043f\u0440\u043e\u0432\u0430\u0439\u0434\u0435\u0440 \u0432 URL, ' +
            '\u0438 socket.io-client \u043f\u043e\u0441\u0447\u0438\u0442\u0430\u043b \u0435\u0433\u043e namespace\'\u043e\u043c.\n' +
            '  \u041a\u0430\u043d\u043e\u043d\u0438\u0447\u043d\u044b\u0439 \u0432\u044b\u0437\u043e\u0432 (namespace \u0432 URL \u043d\u0435 \u043a\u043b\u0430\u0434\u0451\u043c, provider \u2014 \u0432 option path):\n' +
            '    const socket = io("sth://' + node + '", {\n' +
            '      path: "' + pathHint + '",\n' +
            '      transports: ["websocket"]\n' +
            '    });\n' +
            '  \u041b\u0438\u0431\u043e \u043e\u0441\u0442\u0430\u0432\u044c\u0442\u0435 URL \u043a\u0430\u043a \u0435\u0441\u0442\u044c, \u043d\u043e \u043d\u0430 \u0441\u0435\u0440\u0432\u0435\u0440\u0435 \u0437\u0430\u0440\u0435\u0433\u0438\u0441\u0442\u0440\u0438\u0440\u0443\u0439\u0442\u0435 io.of("/' + prov + '"). ' +
            '\u041f\u043e\u0434\u0440\u043e\u0431\u043d\u0435\u0435: docs/08-dApp-Networking-Guide.md \u2192 \u00abWebSocket \u0438 socket.io \u0447\u0435\u0440\u0435\u0437 P2P\u00bb \u2192 \u00ab\u041e\u0448\u0438\u0431\u043a\u0430 Invalid namespace\u00bb.';
          tlog(hint, 'error');
        }
        function deliverMessage(payload) {
          var d;
          if (payload.kind === 'text') { d = payload.data; maybePrintNsHint(d); }
          else {
            var raw = atob(payload.data);
            var arr = new Uint8Array(raw.length);
            for (var i = 0; i < raw.length; i++) arr[i] = raw.charCodeAt(i);
            d = (self.binaryType === 'arraybuffer') ? arr.buffer : new Blob([arr]);
          }
          self._fire('message', { type: 'message', data: d });
        }
        function handleMsg(e) {
          if (!e || !e.payload) return;
          if (mySid === null) { pendingEvents.push({ t: 'm', p: e.payload }); return; }
          if (e.payload.streamId !== mySid) return;
          deliverMessage(e.payload);
        }
        function handleClose(e) {
          if (!e || !e.payload) return;
          if (mySid === null) { pendingEvents.push({ t: 'c', p: e.payload }); return; }
          if (e.payload.streamId !== mySid) return;
          self._fireClose(e.payload.code || 1000, e.payload.reason || '');
        }

        Promise.all([
          api.event.listen('dapp-ws-message', handleMsg),
          api.event.listen('dapp-ws-close', handleClose)
        ]).then(function (uns) {
          unlistenMsg = uns[0]; unlistenClose = uns[1];
          return wsBridge('wsOpen', { url: normalized, headers: reqHeaders });
        }).then(function (res) {
          var sid = (res && typeof res === 'object') ? (res.stream_id || res.streamId) : res;
          var status = (res && typeof res === 'object') ? (res.status || 0) : 0;
          if (!sid) {
            self._fire('error', { type: 'error', message: 'wsOpen: пустой stream_id' });
            self._fireClose(1006, 'no stream_id');
            return;
          }
          self._streamId = sid;
          mySid = sid;
          self.readyState = 1;
          tlog('[sth://ws] open \u00b7 stream=' + String(sid).slice(0, 8) + '\u2026 (HTTP ' + status + ')', 'success');
          if (protocols) {
            self.protocol = Array.isArray(protocols) ? (protocols[0] || '') : String(protocols);
          }
          self._fire('open', { type: 'open', status: status });
          // Дренируем буфер: сначала все message нашего sid, затем close (если был).
          var closeEv = null;
          for (var i = 0; i < pendingEvents.length; i++) {
            var ev = pendingEvents[i];
            if (ev.p.streamId !== sid) continue;
            if (ev.t === 'm') deliverMessage(ev.p);
            else if (ev.t === 'c') { closeEv = ev.p; break; }
          }
          pendingEvents = null;
          if (closeEv) self._fireClose(closeEv.code || 1000, closeEv.reason || '');
        }).catch(function (err) {
          var msg = String(err && err.message || err);
          tlog('[sth://ws] error \u00b7 ' + msg, 'error');
          self._fire('error', { type: 'error', message: msg });
          self._fireClose(1006, msg);
        });

        self.send = function (data) {
          if (!self._streamId || self.readyState !== 1) {
            throw new Error('WebSocket is not open');
          }
          var kind, payload;
          if (typeof data === 'string') { kind = 'text'; payload = data; }
          else if (data instanceof ArrayBuffer) {
            kind = 'binary';
            var bytes = new Uint8Array(data);
            var bin = ''; for (var i = 0; i < bytes.length; i++) bin += String.fromCharCode(bytes[i]);
            payload = btoa(bin);
          } else if (ArrayBuffer.isView(data)) {
            kind = 'binary';
            var b2 = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
            var bin2 = ''; for (var j = 0; j < b2.length; j++) bin2 += String.fromCharCode(b2[j]);
            payload = btoa(bin2);
          } else if (data && data instanceof Blob) {
            var r = new FileReader();
            r.onload = function () {
              var ab = r.result;
              var bytes3 = new Uint8Array(ab);
              var bin3 = ''; for (var k = 0; k < bytes3.length; k++) bin3 += String.fromCharCode(bytes3[k]);
              wsBridge('wsSend', { streamId: self._streamId, kind: 'binary', data: btoa(bin3) })
                .catch(function () {});
            };
            r.readAsArrayBuffer(data);
            return;
          } else { kind = 'text'; payload = String(data); }
          wsBridge('wsSend', { streamId: self._streamId, kind: kind, data: payload })
            .catch(function (e) { self._fire('error', { type: 'error', message: String(e) }); });
        };

        self.close = function (code, reason) {
          if (self.readyState === 3) return;
          self.readyState = 2;
          if (!self._streamId) { self._fireClose(code || 1000, reason || ''); return; }
          wsBridge('wsClose', { streamId: self._streamId, code: code || 1000, reason: reason || '' })
            .catch(function () {});
        };

        self.addEventListener = function (type, cb) {
          if (self._listeners[type]) self._listeners[type].push(cb);
        };
        self.removeEventListener = function (type, cb) {
          if (self._listeners[type]) {
            self._listeners[type] = self._listeners[type].filter(function (x) { return x !== cb; });
          }
        };
        self._fire = function (type, evt) {
          try { var h = self['on' + type]; if (typeof h === 'function') h.call(self, evt); } catch (e) {}
          (self._listeners[type] || []).forEach(function (cb) { try { cb.call(self, evt); } catch (e) {} });
        };
        self._fireClose = function (code, reason) {
          if (self.readyState === 3) return;
          self.readyState = 3;
          var evt = { type: 'close', code: code, reason: reason, wasClean: code === 1000 };
          self._fire('close', evt);
          try { if (unlistenOpen) unlistenOpen(); } catch (e) {}
          try { if (unlistenMsg) unlistenMsg(); } catch (e) {}
          try { if (unlistenClose) unlistenClose(); } catch (e) {}
        };
      }
      // v1.55.33: КРИТИЧЕСКИЙ фикс — раньше было `StubWs.prototype = NativeWS.prototype`.
      // У нативного WebSocket.prototype свойства (onopen, readyState, binaryType, url…)
      // — это нативные аксессоры, которые бросают «Illegal invocation» при вызове
      // с не-нативным this. Поэтому `self.onopen = null` в конструкторе кидал
      // TypeError, engine.io ловил его и выдавал «websocket error» в DApp.
      // Теперь: промежуточный прототип, наследующий NativeWS.prototype (instanceof
      // WebSocket сохраняется), но перекрывающий аксессоры обычными writable-полями.
      StubWs.prototype = Object.create(NativeWS.prototype, {
        url: { value: '', writable: true },
        readyState: { value: 0, writable: true },
        protocol: { value: '', writable: true },
        extensions: { value: '', writable: true },
        bufferedAmount: { value: 0, writable: true },
        binaryType: { value: 'blob', writable: true },
        onopen: { value: null, writable: true },
        onmessage: { value: null, writable: true },
        onclose: { value: null, writable: true },
        onerror: { value: null, writable: true },
        constructor: { value: StubWs, writable: true }
      });
      StubWs.CONNECTING = 0; StubWs.OPEN = 1; StubWs.CLOSING = 2; StubWs.CLOSED = 3;
      window.WebSocket = StubWs;
    }
  })();
  window.dispatchEvent(new Event('smartholdem#initialized'));
  tlog('[sth://system] Web3 bridge initialized · ' + APP_ID, 'system');

  // Tell the host the dApp page is ready, so it can reveal the webview and hide
  // the "loading from the decentralized network" overlay.
  function announceLoaded() {
    emit('dapp-loaded', { app_id: APP_ID, title: (document.title || APP_ID) });
    tlog('[sth://system] dApp ready · rendered from P2P network', 'system');
  }
  if (document.readyState === 'complete' || document.readyState === 'interactive') setTimeout(announceLoaded, 0);
  else window.addEventListener('DOMContentLoaded', announceLoaded);

  // ---- crypto self-test: log which Web Crypto primitives are available so dApp
  // developers can debug cross-platform (sth://) compatibility at a glance.
  (function () {
    try {
      var c = window.crypto, s = c && c.subtle;
      var polyfilled = !!window.__sthCryptoPolyfilled;
      var mode = polyfilled ? 'POLYFILL (sth:// fallback)' : 'NATIVE (secure context)';
      function fn(o, k) { return o && typeof o[k] === 'function' ? 'ok' : 'MISSING'; }
      var report = {
        mode: mode,
        getRandomValues: fn(c, 'getRandomValues'),
        randomUUID: fn(c, 'randomUUID'),
        'subtle.digest': fn(s, 'digest'),
        'subtle.importKey': fn(s, 'importKey'),
        'subtle.sign': fn(s, 'sign'),
        'subtle.verify': fn(s, 'verify'),
        'subtle.deriveBits': fn(s, 'deriveBits'),
        'subtle.deriveKey': fn(s, 'deriveKey'),
        'subtle.encrypt': fn(s, 'encrypt'),
        'subtle.decrypt': fn(s, 'decrypt')
      };
      window.smartholdem.cryptoReport = report;
      console.log('%c[smartholdem] crypto self-test · ' + mode, 'color:#22c55e;font-weight:bold');
      if (console.table) console.table(report); else console.log(report);
      tlog('[sth://crypto] engine ' + mode, polyfilled ? 'info' : 'success');
      function emitReport(integrity) {
        report.integrity = integrity;
        window.smartholdem.cryptoReport = report;
        emit('dapp-crypto-report', { app_id: APP_ID, polyfilled: polyfilled, mode: mode, integrity: integrity, report: report });
      }
      // functional probe: SHA-256("abc") must equal the known digest.
      if (s && typeof s.digest === 'function') {
        s.digest('SHA-256', new Uint8Array([97, 98, 99])).then(function (h) {
          var b = new Uint8Array(h), hex = '';
          for (var i = 0; i < b.length; i++) hex += (b[i] + 0x100).toString(16).slice(1);
          var pass = hex === 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad';
          console.log('[smartholdem] SHA-256("abc") ' + (pass ? 'verified ✓' : 'MISMATCH ✗ -> ' + hex));
          tlog('[sth://crypto] SHA-256 integrity check: ' + (pass ? 'PASSED' : 'FAILED'), pass ? 'success' : 'error');
          emitReport(pass);
        }).catch(function (e) { tlog('[sth://crypto] digest probe failed: ' + e, 'error'); emitReport(false); });
      } else {
        tlog('[sth://crypto] subtle.digest unavailable', 'error');
        emitReport(false);
      }
    } catch (e) { /* diagnostics must never break the dApp */ }
  })();
})();
"#;

/// Build the dApp bridge initialization script for a specific app id. Injects
/// `window.__STH_APP_ID__` (the host/authority is platform-dependent) before the
/// bridge body so terminal logs / `dapp-loaded` / crypto-report key by the real
/// address on every platform. Also substitutes `__STH_CLIENT_VERSION__` inside
/// the bridge with the real Cargo package version — dApps can read it via
/// `window.__SMARTNET__.clientVersion` for feature-gating.
pub(crate) fn dapp_bridge_script(app_id: &str) -> String {
    let id = serde_json::to_string(app_id).unwrap_or_else(|_| "\"\"".to_string());
    let body = DAPP_BRIDGE_JS.replace("__STH_CLIENT_VERSION__", env!("CARGO_PKG_VERSION"));
    format!("window.__STH_APP_ID__ = {};\n{}", id, body)
}

/// Emit a `dapp-response` resolution for a pending bridge request (app-wide; the
/// injected listener filters by id).
pub(crate) fn emit_dapp_response(app: &tauri::AppHandle, id: &str, result: Option<serde_json::Value>, error: Option<String>) {
    crate::syslog::emit_queued(app, "dapp-response", serde_json::json!({ "id": id, "result": result, "error": error }));
}

/// Handle one `window.smartholdem.*` request (from the event channel or the
/// fallback command). Approval-gated methods surface on the host Verify view.
pub(crate) fn process_dapp_request(
    app: &tauri::AppHandle,
    id: String,
    method: String,
    params: serde_json::Value,
    origin: String,
) {
    match method.as_str() {
        // Connect request (getAccount): surface a connect-permission prompt on
        // the shell. The shell auto-resolves for already-trusted origins.
        //
        // We intentionally do NOT force-focus the main window here — earlier
        // versions did that to make the prompt visible when a dApp lives in
        // its own OS window, but stealing focus messed up the z-order of
        // detached dApp webviews (they ended up rendered above the main
        // window's UI). The overlay painted inside the dApp webview by
        // DAPP_BRIDGE_JS (`incoming-connect-request`) is enough to tell the
        // user where the prompt is; they click through to bring main forward.
        "getAccount" => {
            let address = crate::load_vault_internal().map(|v| v.address).unwrap_or_default();
            if address.is_empty() {
                emit_dapp_response(app, &id, None, Some("Wallet is locked".into()));
            } else {
                crate::syslog::emit_queued(app, "incoming-connect-request", serde_json::json!({ "id": id, "origin": origin }));
            }
        }
        // Approval-gated: sign an arbitrary message (login-with-wallet). The
        // request id IS the dApp bridge id, so the Approve/Reject path on the
        // shell resolves the dApp Promise via `dapp-response`. This mirrors the
        // Core-Wallet browser extension `AuthorizeMessage` flow — the user MUST
        // explicitly consent (unless the origin was already trusted at
        // `getAccount` handshake, in which case the frontend auto-approves).
        "signMessage" => {
            let secret = SESSION_SEED.lock().unwrap().clone();
            if secret.is_none() {
                emit_dapp_response(app, &id, None, Some("Wallet is locked".into()));
                return;
            }
            let message = params["message"].as_str().unwrap_or("").to_string();
            if message.is_empty() {
                emit_dapp_response(app, &id, None, Some("signMessage: empty message".into()));
                return;
            }
            // Focusing the main window is intentionally done on the frontend
            // (only when we actually need to surface the prompt — not for
            // trusted origins that auto-approve), otherwise the shell would
            // steal focus on every silent auto-signature.
            let payload = serde_json::json!({
                "requestId": id,
                "origin": origin,
                "message": message,
            });
            crate::syslog::emit_queued(app, "incoming-sign-message-request", payload);
        }
        // Approval-gated: surface the request on the shell's Verify view. The
        // request id IS the dApp bridge id, so the Approve/Reject path resolves
        // the dApp Promise via `dapp-response`.
        "signTransaction" | "sendTransaction" => {
            let recipient = params["recipientId"]
                .as_str()
                .or_else(|| params["recipient"].as_str())
                .unwrap_or("");
            if recipient.is_empty() {
                emit_dapp_response(app, &id, None, Some("signTransaction: missing `recipientId`".into()));
                return;
            }
            let amount = value_to_str(&params["amount"]);
            let fee = if params["fee"].is_null() {
                "0.25".to_string()
            } else {
                value_to_str(&params["fee"])
            };
            let memo = params["vendorField"].as_str().unwrap_or("").to_string();
            let payload = serde_json::json!({
                "requestId": id,
                "origin": origin,
                "recipient": recipient,
                "amount": amount,
                "fee": fee,
                "memo": memo,
                "method": method,
            });
            crate::syslog::emit_queued(app, "incoming-tx-request", payload);
        }
        // Deep-link into the in-wallet Swap hub. Resolves immediately.
        "requestSwap" => {
            crate::syslog::emit_queued(app, "incoming-swap-request", params.clone());
            emit_dapp_response(app, &id, Some(serde_json::Value::Null), None);
        }
        // Раскрыть WalletDrawer в host-shell (пункт меню «Wallet» в dApp'е).
        // Не approval-gated: панель просто выезжает, никаких подписей.
        "openWallet" => {
            crate::syslog::emit_queued(app, "dapp-open-wallet", serde_json::json!({ "origin": origin }));
            emit_dapp_response(app, &id, Some(serde_json::Value::Null), None);
        }
        // Прозрачный P2P HTTP-транспорт для dApp'ов: fetch-polyfill в bridge JS
        // роутит через этот метод любые `fetch('api://...')`, потому что
        // WebkitGTK/WKWebView блокируют cross-scheme fetch (sth:// → api://)
        // по CORS ещё ДО того, как наш uri-scheme handler отвечает. Здесь мы
        // выполняем реальный P2P-запрос через `p2p::api_request`, дергаем из
        // envelope `pdata.body_b64` и отдаём dApp'у СЫРОЕ тело апстрима вместе
        // с HTTP-статусом и заголовками sig-verified — dApp получает
        // ровно тот JSON, что вернул бы REST напрямую.
        "apiFetch" => {
            let url = params["url"].as_str().unwrap_or("").to_string();
            let method = params["method"].as_str().unwrap_or("GET").to_string();
            let body = params["body"].as_str().unwrap_or("").to_string();
            // Заголовки от dApp'а (Authorization, X-Custom-…) — прокидываем
            // в mesh-packet, чтобы провайдер их форварднул апстриму. `params.headers`
            // приходит как `{ "authorization": "Bearer …", ... }` (см. bridge JS).
            let hdrs: std::collections::BTreeMap<String, String> = params
                .get("headers")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            if url.is_empty() || !url.starts_with("api://") {
                emit_dapp_response(app, &id, None, Some("apiFetch: url must start with api://".into()));
                return;
            }
            #[cfg(feature = "p2p")]
            {
                let app2 = app.clone();
                let id2 = id.clone();
                tauri::async_runtime::spawn(async move {
                    match crate::p2p::api_request(app2.clone(), url, method, body, hdrs).await {
                        Err(e) => emit_dapp_response(&app2, &id2, None, Some(e)),
                        Ok(r) => {
                            use base64::Engine;
                            // Извлекаем сырое тело из pdata.body_b64 (тот же
                            // алгоритм, что и в `api://` uri-handler).
                            let (body_str, sniffed_ct) =
                                match serde_json::from_str::<serde_json::Value>(&r.body) {
                                    Ok(v) => {
                                        let b64 = v
                                            .get("pdata")
                                            .and_then(|p| p.get("body_b64"))
                                            .and_then(|s| s.as_str())
                                            .unwrap_or("");
                                        let raw = if b64.is_empty() {
                                            r.body.clone()
                                        } else {
                                            base64::engine::general_purpose::STANDARD
                                                .decode(b64.as_bytes())
                                                .ok()
                                                .and_then(|b| String::from_utf8(b).ok())
                                                .unwrap_or_else(|| r.body.clone())
                                        };
                                        let ct = raw
                                            .as_bytes()
                                            .iter()
                                            .find(|b| !b.is_ascii_whitespace())
                                            .map(|b| match b {
                                                b'{' | b'[' => "application/json; charset=utf-8",
                                                b'<' => "text/html; charset=utf-8",
                                                _ => "text/plain; charset=utf-8",
                                            })
                                            .unwrap_or("application/octet-stream");
                                        (raw, ct.to_string())
                                    }
                                    Err(_) => (r.body.clone(), "text/plain; charset=utf-8".to_string()),
                                };
                            let status = if r.status == 0 { 200 } else { r.status };
                            let result = serde_json::json!({
                                "status": status,
                                "body": body_str,
                                "contentType": sniffed_ct,
                                "sigVerified": r.verified,
                                "provider": r.provider_node_id,
                                "signedAt": r.signed_at,
                                "relayed": r.relayed,
                                "target": r.target,
                            });
                            emit_dapp_response(&app2, &id2, Some(result), None);
                        }
                    }
                });
            }
            #[cfg(not(feature = "p2p"))]
            emit_dapp_response(app, &id, None, Some("api:// доступен только в десктоп-сборке".into()));
        }
        // WS-туннель через событийный канал (v1.55.36): remote-origin webview'ы
        // DApp'ов НЕ МОГУТ вызывать invoke-команды (Tauri ACL всегда режет
        // команды приложения для remote URL — «not allowed by ACL»). Bridge JS
        // шлёт wsOpen/wsSend/wsClose через `dapp-request`, как и apiFetch.
        "wsOpen" => {
            let url = params["url"].as_str().unwrap_or("").to_string();
            let hdrs: std::collections::BTreeMap<String, String> = params
                .get("headers")
                .and_then(|v| v.as_object())
                .map(|obj| {
                    obj.iter()
                        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            if url.is_empty() {
                emit_dapp_response(app, &id, None, Some("wsOpen: пустой url".into()));
                return;
            }
            let app2 = app.clone();
            let id2 = id.clone();
            tauri::async_runtime::spawn(async move {
                match crate::p2p::dapp_ws_open(app2.clone(), url, hdrs).await {
                    Ok(r) => emit_dapp_response(
                        &app2,
                        &id2,
                        Some(serde_json::to_value(&r).unwrap_or(serde_json::Value::Null)),
                        None,
                    ),
                    Err(e) => emit_dapp_response(&app2, &id2, None, Some(e)),
                }
            });
        }
        "wsSend" => {
            let stream_id = params["streamId"].as_str().unwrap_or("").to_string();
            let kind = params["kind"].as_str().unwrap_or("text").to_string();
            let data = params["data"].as_str().unwrap_or("").to_string();
            if stream_id.is_empty() {
                emit_dapp_response(app, &id, None, Some("wsSend: пустой streamId".into()));
                return;
            }
            let app2 = app.clone();
            let id2 = id.clone();
            tauri::async_runtime::spawn(async move {
                match crate::p2p::dapp_ws_send(stream_id, kind, data).await {
                    Ok(_) => emit_dapp_response(&app2, &id2, Some(serde_json::Value::Null), None),
                    Err(e) => emit_dapp_response(&app2, &id2, None, Some(e)),
                }
            });
        }
        "wsClose" => {
            let stream_id = params["streamId"].as_str().unwrap_or("").to_string();
            let code = params["code"].as_u64().map(|c| c as u16);
            let reason = params["reason"].as_str().map(|s| s.to_string());
            if stream_id.is_empty() {
                emit_dapp_response(app, &id, Some(serde_json::Value::Null), None);
                return;
            }
            let app2 = app.clone();
            let id2 = id.clone();
            tauri::async_runtime::spawn(async move {
                match crate::p2p::dapp_ws_close(stream_id, code, reason).await {
                    Ok(_) => emit_dapp_response(&app2, &id2, Some(serde_json::Value::Null), None),
                    Err(e) => emit_dapp_response(&app2, &id2, None, Some(e)),
                }
            });
        }
        // --- Netfory AI Compute: window.smartholdem.useAI() ------------
        // Стриминговая генерация: токены летят в dApp событиями
        // `dapp-ai-token` {streamId, t}; финал резолвит Promise через
        // dapp-response. Исполнителя выбирает диспетчер AI-трекера.
        "aiGenerate" => {
            let prompt = params["prompt"].as_str().unwrap_or("").to_string();
            if prompt.is_empty() {
                emit_dapp_response(app, &id, None, Some("useAI: пустой prompt".into()));
                return;
            }
            let stream_id = params["streamId"].as_str().unwrap_or(&id).to_string();
            let model = params["model"].as_str().filter(|s| !s.is_empty()).map(|s| s.to_string());
            let max = params["maxTokens"].as_u64().unwrap_or(256).clamp(1, 2048) as usize;
            let think = params["think"].as_bool().unwrap_or(true);
            #[cfg(feature = "p2p")]
            crate::ai_client::spawn_generate(app.clone(), id, stream_id, prompt, model, max, think);
            #[cfg(not(feature = "p2p"))]
            {
                let _ = (stream_id, model, max, think);
                emit_dapp_response(app, &id, None, Some("useAI доступен только в десктоп-сборке".into()));
            }
        }
        "aiCancel" => {
            let sid = params["streamId"].as_str().unwrap_or("").to_string();
            #[cfg(feature = "p2p")]
            if !sid.is_empty() {
                crate::ai_client::cancel(app, &sid);
            }
            #[cfg(not(feature = "p2p"))]
            let _ = sid;
            emit_dapp_response(app, &id, Some(serde_json::Value::Null), None);
        }
        other => emit_dapp_response(app, &id, None, Some(format!("unknown method: {}", other))),
    }
}

/// Fallback command (used by the local host UI / browser mock). Remote dApps use
/// the event channel instead, since they cannot invoke commands.
#[tauri::command]
pub(crate) fn dapp_provider_request(
    app: tauri::AppHandle,
    id: String,
    method: String,
    params: serde_json::Value,
    origin: String,
) -> Result<(), String> {
    process_dapp_request(&app, id, method, params, origin);
    Ok(())
}

/// Approve a dApp connect (getAccount) request — resolves the dApp Promise with
/// the active address. Called by the host shell after the user grants access.
#[tauri::command]
pub(crate) fn resolve_dapp_connect(app: tauri::AppHandle, id: String) -> Result<(), String> {
    let address = crate::load_vault_internal().map(|v| v.address).unwrap_or_default();
    if address.is_empty() {
        emit_dapp_response(&app, &id, None, Some("Wallet is locked".into()));
    } else {
        emit_dapp_response(&app, &id, Some(serde_json::json!({ "address": address })), None);
    }
    Ok(())
}

/// Approve a `signMessage` request after the user pressed "Sign" in the
/// AuthorizeMessage view. Delegates to `sth_node::sign_message` (bip-schnorr
/// legacy consensus), then resolves the dApp Promise with the canonical
/// `{ address, publicKey, hash, message, signature }` envelope Core-Wallet uses.
#[tauri::command]
pub(crate) fn resolve_sign_message(
    app: tauri::AppHandle,
    request_id: String,
    message: String,
) -> Result<(), String> {
    let secret = match SESSION_SEED.lock().unwrap().clone() {
        Some(s) => s,
        None => {
            emit_dapp_response(&app, &request_id, None, Some("Wallet is locked".into()));
            return Err("Wallet is locked".into());
        }
    };
    match sth_node::sign_message(&secret, &message) {
        Ok((public_key, hash, signature)) => {
            let address = crate::load_vault_internal().map(|v| v.address).unwrap_or_default();
            emit_dapp_response(
                &app,
                &request_id,
                Some(serde_json::json!({
                    "address": address,
                    "publicKey": public_key,
                    "hash": hash,
                    "message": message,
                    "signature": signature,
                })),
                None,
            );
            Ok(())
        }
        Err(e) => {
            emit_dapp_response(&app, &request_id, None, Some(e.clone()));
            Err(e)
        }
    }
}

/// Reject a pending `signMessage` request — signals failure back to the dApp.
#[tauri::command]
pub(crate) fn reject_sign_message(app: tauri::AppHandle, request_id: String) -> Result<(), String> {
    emit_dapp_response(
        &app,
        &request_id,
        None,
        Some("User rejected the signature request.".into()),
    );
    Ok(())
}

pub(crate) fn value_to_str(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        _ => "0".to_string(),
    }
}


// ============================================================
//  Перевод страницы DApp — content-скрипт (браузерный DOM-перевод).
//  Инжектится как init-скрипт. Управляется из главного окна через
//  window.__sthI18n.set(on, target). Переводит видимую область + по скроллу
//  (IntersectionObserver) и динамический контент (MutationObserver, debounce).
//  Обмен с бэкендом — события dapp-translate-request / dapp-translate-response.
// ============================================================
const DAPP_I18N_JS: &str = r#"
(function () {
  'use strict';
  if (window.__sthI18n) return;
  function T() { try { return window.__TAURI__; } catch (e) { return null; } }
  var SKIP = { SCRIPT:1, STYLE:1, NOSCRIPT:1, CODE:1, PRE:1, TEXTAREA:1, KBD:1, SAMP:1, OPTION:1 };
  var TR_ATTRS = ['title', 'placeholder', 'alt', 'aria-label'];
  // `selOn: true` по умолчанию → правый клик на выделении СРАЗУ показывает
  // «Перевести на мой язык» даже если хост-окно ещё не успело позвать
  // `config(target, selOn, label)`. Раньше при поздней инициализации хоста
  // menu не появлялся, а WebKit/GTK показывал свою «…»-plate над выделением.
  // Язык (`target`) и лейбл (`selLabel`) подставляются позже из хоста.
  var state = { on: false, target: 'en', selOn: true, selLabel: 'Перевести на мой язык' };
  // Бюджеты против «вечного перевода» на анимированных страницах (тикеры цен,
  // живые карты): ограничиваем общий объём и число строк, добавленных из
  // MutationObserver, чтобы постоянно меняющийся DOM не гонял перевод бесконечно.
  var MAX_TOTAL = 4000;
  var MO_MAX = 600;
  var SID = Math.random().toString(36).slice(2, 8);
  var reqSeq = 0, pending = {}, pendingCount = 0, queue = [], flushTimer = null, io = null, mo = null, moTimer = null, progTimer = null;
  var moRoots = [], moEnqueued = 0;
  var originals = new WeakMap(), tracked = [];
  var stats = { total: 0, done: 0, cache: 0, start: 0, end: 0 };
  var startedLogged = false, lastDoneCount = 0;

  function isTranslatable(txt) {
    if (!txt) return false;
    var s = ('' + txt).trim();
    if (s.length < 2 || s.length > 5000) return false;
    if (s.indexOf('...') >= 0 || s.indexOf('\u2026') >= 0) return false;
    if (!/\s/.test(s)) {
      if (/\d/.test(s) && /[A-Za-z]/.test(s)) return false;
      if (/^[0-9a-fA-F]{8,}$/.test(s)) return false;
      if (/^S[1-9A-HJ-NP-Za-km-z]{25,}$/.test(s)) return false;
      if (/^[A-Za-z\u0400-\u04FF0-9]{1,6}$/.test(s) && s === s.toUpperCase()) return false;
      if (s.length > 24 && /^[A-Za-z0-9+/=_.-]+$/.test(s)) return false;
    }
    if (!/[A-Za-z\u00C0-\u024F\u0400-\u04FF\u4e00-\u9fff]/.test(s)) return false;
    if (/^[\d\s.,:%+\-\/()#$€₽*]+$/.test(s)) return false;
    return true;
  }
  function collectShallow(el) {
    var items = [];
    if (!el || el.nodeType !== 1) return items;
    if (!SKIP[el.nodeName] && !el.isContentEditable) {
      for (var c = el.firstChild; c; c = c.nextSibling) {
        if (c.nodeType === 3 && !c.__sthDone && isTranslatable(c.nodeValue)) items.push({ k: 't', n: c });
      }
    }
    for (var a = 0; a < TR_ATTRS.length; a++) {
      var at = TR_ATTRS[a];
      if (el.hasAttribute && el.hasAttribute(at) && !(el.__sthA && el.__sthA[at]) && isTranslatable(el.getAttribute(at))) {
        items.push({ k: 'a', e: el, at: at });
      }
    }
    return items;
  }
  // Собрать переводимые строки в поддереве добавленного узла (с лимитом).
  function collectSubtree(root, out, budget) {
    if (!root || root.nodeType !== 1 || out.length >= budget) return;
    var got = collectShallow(root);
    for (var i = 0; i < got.length; i++) { out.push(got[i]); if (out.length >= budget) return; }
    var kids = root.children || [];
    for (var j = 0; j < kids.length; j++) { collectSubtree(kids[j], out, budget); if (out.length >= budget) return; }
  }  function hasWork(el) {
    if (!SKIP[el.nodeName] && !el.isContentEditable) {
      for (var c = el.firstChild; c; c = c.nextSibling) {
        if (c.nodeType === 3 && !c.__sthDone && isTranslatable(c.nodeValue)) return true;
      }
    }
    for (var a = 0; a < TR_ATTRS.length; a++) {
      var at = TR_ATTRS[a];
      if (el.hasAttribute && el.hasAttribute(at) && !(el.__sthA && el.__sthA[at]) && isTranslatable(el.getAttribute(at))) return true;
    }
    return false;
  }
  function itemOrig(it) { return it.k === 't' ? originals.get(it.n) : (it.e.__sthAO && it.e.__sthAO[it.at]) || ''; }
  // Сохранить ведущие/замыкающие пробелы узла: заголовки часто разбиты на
  // inline-<span>, и обрезка пробелов при переводе слепляет соседние слова.
  function leadTrail(s) { var m = /^(\s*)[\s\S]*?(\s*)$/.exec('' + (s || '')); return m ? [m[1], m[2]] : ['', '']; }
  function markItem(it) {
    if (it.k === 't') {
      if (it.n.__sthDone) return false;
      it.n.__sthDone = true;
      if (!originals.has(it.n)) originals.set(it.n, it.n.nodeValue);
    } else {
      it.e.__sthA = it.e.__sthA || {}; if (it.e.__sthA[it.at]) return false;
      it.e.__sthA[it.at] = true; it.e.__sthAO = it.e.__sthAO || {}; it.e.__sthAO[it.at] = it.e.getAttribute(it.at);
    }
    return true;
  }
  function applyItem(it, tr) {
    try {
      if (it.k === 't') {
        if (it.n.parentNode) {
          var lt = leadTrail(originals.get(it.n));
          it.n.nodeValue = lt[0] + tr + lt[1];
        }
      } else { it.e.setAttribute(it.at, tr); }
    } catch (e) {}
  }
  function restoreItem(it) {
    try {
      if (it.k === 't') { if (it.n.parentNode) it.n.nodeValue = originals.get(it.n); it.n.__sthDone = false; }
      else { if (it.e.__sthAO) it.e.setAttribute(it.at, it.e.__sthAO[it.at]); if (it.e.__sthA) it.e.__sthA[it.at] = false; }
    } catch (e) {}
  }
  function enqueue(items) {
    var added = 0;
    for (var i = 0; i < items.length; i++) {
      if (stats.total >= MAX_TOTAL) break;
      if (markItem(items[i])) { queue.push(items[i]); tracked.push(items[i]); added++; }
    }
    if (added) {
      if (!stats.start) stats.start = Date.now();
      stats.total += added; stats.end = 0;
      emitProgress(); scheduleFlush();
    }
  }
  function scheduleFlush() { if (!flushTimer) flushTimer = setTimeout(flush, 150); }
  function flush() {
    flushTimer = null;
    if (!state.on || !queue.length) return;
    var batch = queue.splice(0, 50);
    var texts = batch.map(function (it) { return ('' + itemOrig(it)).trim(); });
    var id = SID + (++reqSeq); pending[id] = batch; pendingCount++;
    if (!startedLogged) { startedLogged = true; tlog('[sth://i18n] translation started → ' + state.target.toUpperCase()); }
    var api = T();
    if (api && api.event) api.event.emit('dapp-translate-request', { id: id, texts: texts, target: state.target });
    if (queue.length) scheduleFlush();
  }
  function applyResponse(id, translations, fromCache) {
    var batch = pending[id]; if (!batch) return;
    delete pending[id]; pendingCount = Math.max(0, pendingCount - 1); fromCache = fromCache || [];
    for (var i = 0; i < batch.length; i++) {
      stats.done++; if (fromCache[i]) stats.cache++;
      if (state.on && translations[i]) applyItem(batch[i], translations[i]);
    }
    emitProgress();
  }
  function running() { return queue.length > 0 || pendingCount > 0; }
  function emitProgress() {
    if (progTimer) return;
    progTimer = setTimeout(function () {
      progTimer = null;
      var run = running();
      if (!run && !stats.end) stats.end = Date.now();
      if (!run && stats.done > lastDoneCount) {
        lastDoneCount = stats.done;
        var secs = ((stats.end - stats.start) / 1000).toFixed(1);
        var pctCache = stats.done ? Math.round((stats.cache / stats.done) * 100) : 0;
        tlog('[sth://i18n] translation done · ' + stats.done + ' strings · ' + pctCache + '% cache · ' + secs + 's');
      }
      var api = T();
      if (api && api.event) api.event.emit('dapp-translate-progress', {
        total: stats.total, done: stats.done, cache: stats.cache,
        running: run, elapsedMs: (stats.end || Date.now()) - (stats.start || Date.now())
      });
    }, 120);
  }
  function scan() {
    if (!state.on || !document.body) return;
    if (io) io.disconnect();
    io = new IntersectionObserver(function (entries) {
      var items = [];
      for (var i = 0; i < entries.length; i++) {
        if (entries[i].isIntersecting) { items = items.concat(collectShallow(entries[i].target)); io.unobserve(entries[i].target); }
      }
      if (items.length) enqueue(items);
    }, { rootMargin: '150px' });
    var els = document.body.querySelectorAll('*'), n = 0;
    for (var i = 0; i < els.length; i++) {
      if (hasWork(els[i])) { io.observe(els[i]); n++; if (n > 5000) break; }
    }
  }
  function startMO() {
    if (mo) return;
    // Реагируем ТОЛЬКО на добавленные узлы (не полный ре-скан), с дебаунсом и
    // бюджетом MO_MAX — иначе анимированный DOM (тикеры/карты) переводится вечно.
    mo = new MutationObserver(function (muts) {
      if (moEnqueued >= MO_MAX) { try { mo.disconnect(); } catch (e) {} return; }
      for (var i = 0; i < muts.length; i++) {
        var an = muts[i].addedNodes || [];
        for (var j = 0; j < an.length; j++) { if (an[j].nodeType === 1) moRoots.push(an[j]); }
      }
      if (moRoots.length > 4000) moRoots.splice(0, moRoots.length - 4000);
      if (moTimer) return;
      moTimer = setTimeout(function () {
        moTimer = null;
        if (!state.on) { moRoots = []; return; }
        var roots = moRoots; moRoots = [];
        var items = [];
        var budget = Math.min(300, MO_MAX - moEnqueued);
        for (var i = 0; i < roots.length && items.length < budget; i++) {
          if (roots[i].isConnected) collectSubtree(roots[i], items, budget);
        }
        if (items.length) { moEnqueued += items.length; enqueue(items); }
        if (moEnqueued >= MO_MAX) { try { mo.disconnect(); } catch (e) {} tlog('[sth://i18n] dynamic-content translation paused (animated page)'); }
      }, 700);
    });
    try { mo.observe(document.body, { childList: true, subtree: true }); } catch (e) {}
  }
  function tlog(text) {
    var api = T();
    if (api && api.event) api.event.emit('dapp-terminal-log', { app_id: window.__STH_APP_ID__ || '', text: text, type: 'system' });
  }
  function restore() {
    for (var i = 0; i < tracked.length; i++) restoreItem(tracked[i]);
    tracked = []; queue = []; pending = {}; pendingCount = 0;
    stats = { total: 0, done: 0, cache: 0, start: 0, end: 0 };
    startedLogged = false; lastDoneCount = 0;
    try { if (mo) mo.disconnect(); } catch (e) {}
    mo = null; moRoots = []; moEnqueued = 0;
    var api = T();
    if (api && api.event) api.event.emit('dapp-translate-progress', { total: 0, done: 0, cache: 0, running: false, elapsedMs: 0 });
  }
  function bindResponses() {
    var api = T();
    if (!api || !api.event || window.__sthI18nBound) return;
    window.__sthI18nBound = true;
    api.event.listen('dapp-translate-response', function (e) {
      var m = e.payload || {}; if (!m.id) return;
      if (selPending[m.id]) applySelResponse(m.id, (m.translations || [])[0]);
      else applyResponse(m.id, m.translations || [], m.from_cache || []);
    });
  }
  // --- Перевод по выделению (контекстное меню) ---
  var selBound = false, selMenu = null, selTip = null, selSeq = 0, selPending = {};
  function removeSelMenu() { if (selMenu) { selMenu.remove(); selMenu = null; } }
  function removeSelTip() { if (selTip) { selTip.remove(); selTip = null; } }
  function selText() { try { return ('' + window.getSelection()).trim(); } catch (e) { return ''; } }
  function showSelTip(x, y, text, loading) {
    removeSelTip();
    selTip = document.createElement('div');
    selTip.setAttribute('data-sth-i18n', '1');
    selTip.style.cssText = 'position:fixed;z-index:2147483647;max-width:360px;left:' + x + 'px;top:' + (y + 8) + 'px;background:#0d1524;color:#dbe7ff;border:1px solid #00e5ff;border-radius:8px;padding:8px 10px;font:13px/1.4 system-ui,sans-serif;box-shadow:0 6px 22px rgba(0,0,0,.55);white-space:pre-wrap;';
    selTip.textContent = loading ? '…' : text;
    document.body.appendChild(selTip);
  }
  function applySelResponse(id, tr) {
    var meta = selPending[id]; delete selPending[id];
    if (!meta) return;
    showSelTip(meta.x, meta.y, tr || meta.text, false);
  }
  function translateSelection(x, y) {
    var s = selText(); if (!s) return;
    var id = SID + 'S' + (++selSeq); selPending[id] = { x: x, y: y, text: s };
    showSelTip(x, y, '', true);
    var api = T();
    // `manual: true` → Rust-сторона (translator) форсирует инференс даже на
    // CPU-сборке: для одиночного выделения из контекстного меню пользователь
    // готов подождать 2–5 сек (в отличие от авто-перевода всей страницы,
    // где CPU был бы неприемлемо медленным).
    if (api && api.event) api.event.emit('dapp-translate-request', { id: id, texts: [s], target: state.target, manual: true });
  }
  function buildSelMenu(x, y) {
    removeSelMenu();
    selMenu = document.createElement('div');
    selMenu.setAttribute('data-sth-i18n', '1');
    selMenu.style.cssText = 'position:fixed;z-index:2147483647;left:' + x + 'px;top:' + y + 'px;background:#0d1524;border:1px solid #26314a;border-radius:8px;padding:5px;box-shadow:0 8px 26px rgba(0,0,0,.6);';
    var item = document.createElement('div');
    item.textContent = '\uD83C\uDF10 ' + (state.selLabel || 'Translate selection');
    item.style.cssText = 'padding:7px 12px;color:#dbe7ff;font:13px system-ui,sans-serif;cursor:pointer;border-radius:5px;white-space:nowrap;';
    item.onmouseenter = function () { item.style.background = 'rgba(0,229,255,.12)'; };
    item.onmouseleave = function () { item.style.background = 'transparent'; };
    item.onmousedown = function (ev) { ev.preventDefault(); ev.stopPropagation(); var sx = x, sy = y; removeSelMenu(); translateSelection(sx, sy); };
    selMenu.appendChild(item);
    document.body.appendChild(selMenu);
  }
  function onCtxMenu(e) {
    if (!state.selOn) return;
    if (!selText()) { removeSelMenu(); removeSelTip(); return; }
    e.preventDefault();
    buildSelMenu(e.clientX, e.clientY);
  }
  document.addEventListener('mousedown', function (e) {
    if (selMenu && !selMenu.contains(e.target)) removeSelMenu();
    if (selTip && !selTip.contains(e.target)) removeSelTip();
  }, true);
  document.addEventListener('scroll', function () { removeSelMenu(); removeSelTip(); }, true);
  function enableSelection() {
    if (selBound) return; selBound = true;
    document.addEventListener('contextmenu', onCtxMenu, true);
  }
  function disableSelection() {
    if (!selBound) return; selBound = false;
    document.removeEventListener('contextmenu', onCtxMenu, true);
    removeSelMenu(); removeSelTip();
  }
  function whenReady(fn) { if (document.body) fn(); else document.addEventListener('DOMContentLoaded', fn, { once: true }); }
  window.__sthI18n = {
    set: function (on, target) {
      if (target) state.target = target;
      bindResponses();
      if (on && !state.on) { state.on = true; whenReady(function () { scan(); startMO(); }); }
      else if (!on && state.on) { state.on = false; if (io) io.disconnect(); restore(); tlog('[sth://i18n] translation disabled'); }
    },
    // Настройка (из главного окна, только для GPU-сборок): язык + перевод по выделению.
    config: function (target, selOn, label) {
      if (target) state.target = target;
      if (label) state.selLabel = label;
      state.selOn = !!selOn;
      bindResponses();
      if (state.selOn) whenReady(enableSelection);
      else disableSelection();
    },
    active: function () { return state.on; }
  };
  try { bindResponses(); } catch (e) {}
  // Автоматически включаем перевод по выделению сразу, ещё до того как хост
  // вызовет `config()`. Правый клик на выделении будет работать даже если
  // dApp-loaded event ещё не долетел до главного окна (гоночное условие
  // на медленных сборках). Если хост позже пришлёт `selOn: false` —
  // отключим.
  whenReady(enableSelection);
})();
"#;

pub(crate) fn dapp_i18n_script() -> String {
    DAPP_I18N_JS.to_string()
}
