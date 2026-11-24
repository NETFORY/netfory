import hashlib

# secp256k1
P = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEFFFFFC2F
N = 0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141
Gx = 0x79BE667EF9DCBBAC55A06295CE870B07029BFCDB2DCE28D959F2815B16F81798
Gy = 0x483ADA7726A3C4655DA4FBFC0E1108A8FD17B448A68554199C47D08FFB10D4B8

def inv(a, m): return pow(a, m-2, m)
def add(p1, p2):
    if p1 is None: return p2
    if p2 is None: return p1
    x1,y1=p1; x2,y2=p2
    if x1==x2 and (y1+y2)%P==0: return None
    if p1==p2:
        l=(3*x1*x1)*inv(2*y1,P)%P
    else:
        l=(y2-y1)*inv(x2-x1,P)%P
    x3=(l*l-x1-x2)%P
    y3=(l*(x1-x3)-y1)%P
    return (x3,y3)
def mul(k,pt):
    r=None; a=pt
    while k:
        if k&1: r=add(r,a)
        a=add(a,a); k>>=1
    return r
G=(Gx,Gy)

def decompress(pk):  # 33-byte compressed -> point
    prefix=pk[0]; x=int.from_bytes(pk[1:33],'big')
    y2=(pow(x,3,P)+7)%P
    y=pow(y2,(P+1)//4,P)
    if (y&1)!=(prefix&1): y=P-y
    return (x,y)

def is_square(y):
    return pow(y,(P-1)//2,P)==1

def hashint(*items):
    h=hashlib.sha256()
    for it in items: h.update(it)
    return int.from_bytes(h.digest(),'big')%N

def schnorr_verify(msg, sig, pk):
    r=int.from_bytes(sig[:32],'big'); s=int.from_bytes(sig[32:],'big')
    A=decompress(pk)
    if r>=P or s>=N: return False
    e=hashint(sig[:32], pk, msg)
    R=add(mul(s,G), mul((N-e)%N, A))   # s*G - e*A
    if R is None: return False
    if not is_square(R[1]): return False
    return R[0]==r

# ---- build serialization for the real on-chain tx ----
def b58check_decode(s):
    alphabet=b'123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz'
    num=0
    for c in s.encode():
        num=num*58+alphabet.index(c)
    full=num.to_bytes((num.bit_length()+7)//8,'big')
    # add leading zeros
    pad=len(s)-len(s.lstrip('1'))
    full=b'\x00'*pad+full
    return full[:-4]  # strip 4-byte checksum -> 21 bytes (version+hash)

sender_pk=bytes.fromhex("036560f20d578da7b8433248d0c82e68121163958d533dc74b0e7d8dbabc0606a0")
recipient="SkQEyiNHg7bpyiMNrU93cQ2XWarLUdBiUe"
amount=314159265
fee=100000000
nonce=9878
sig=bytes.fromhex("49478e8f837cb9203ec37a87c927f76f1baded09533b47ce597a53ad184ddb5825019f4e4089d9bc8fe603157f5610c38d8c5088f579460cb750e67424f8251a")
txid="3396438a24d7e8adc72dae81ef2b63fb188a7c6ef0d05ddbc6ef60cbad1728c9"

b=bytearray()
b+=b'\xff'
b+=bytes([2])     # version
b+=bytes([0x3f])  # network
b+=(1).to_bytes(4,'little')   # typeGroup
b+=(0).to_bytes(2,'little')   # type
b+=nonce.to_bytes(8,'little')
b+=sender_pk
b+=fee.to_bytes(8,'little')
b+=bytes([0])     # vendorfield len 0
b+=amount.to_bytes(8,'little')
b+=(0).to_bytes(4,'little')   # expiration
recip=b58check_decode(recipient)
assert len(recip)==21, len(recip)
b+=recip

msg=hashlib.sha256(bytes(b)).digest()
print("serialized(unsigned) len:", len(b))
print("schnorr verify on-chain sig:", schnorr_verify(msg, sig, sender_pk))

full=bytes(b)+sig
computed_id=hashlib.sha256(full).hexdigest()
print("computed id:", computed_id)
print("expected id:", txid)
print("ID MATCH:", computed_id==txid)

# ---- test SIGNING path (mirror of Rust sign_schnorr_legacy) ----
def schnorr_sign(priv32, msg):
    a = int.from_bytes(priv32,'big')
    A = mul(a, G)
    Acomp = bytes([2|(A[1]&1)]) + A[0].to_bytes(32,'big')
    k0 = int.from_bytes(hashlib.sha256(priv32+msg).digest(),'big') % N
    assert k0!=0
    R = mul(k0, G)
    k = k0 if is_square(R[1]) else (N-k0)%N
    rx = R[0].to_bytes(32,'big')
    e = hashint(rx, Acomp, msg)
    s = (k + e*a) % N
    return rx + s.to_bytes(32,'big'), Acomp

import os
for _ in range(5):
    priv = os.urandom(32)
    if int.from_bytes(priv,'big')==0 or int.from_bytes(priv,'big')>=N: continue
    m = hashlib.sha256(os.urandom(40)).digest()
    sig2, pk2 = schnorr_sign(priv, m)
    ok = schnorr_verify(m, sig2, pk2)
    print("sign->verify roundtrip:", ok)
