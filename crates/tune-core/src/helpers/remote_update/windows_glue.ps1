# Fixed definitions only. Never execute source from the header or binary body.
$script:Ktu2Bridge = @"
using System;
using System.IO;
using System.Text;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
using System.Security.Principal;
using System.Security.AccessControl;
using System.Text.RegularExpressions;

public static class Ktu2 {
    internal const ulong Limit = 268435456;
    internal const string Target = "tune-agent.exe", Lock = ".tune-agent-update.lock";
    internal const uint Read = 0x80000000, Write = 0x40000000, Delete = 0x10000, Sync = 0x100000, Control = 0x20000, Attr = 0x80;
    internal const uint NoRecall = 0x400000, NoReparse = 0x200000, Synchronous = 0x20;
    internal const uint UnsafeAttrs = 0x400 | 0x1000 | 0x40000 | 0x400000;
    internal static readonly Encoding Utf8 = new UTF8Encoding(false, true);
    internal static readonly Encoding Utf16 = new UnicodeEncoding(false, false, true);
    internal class Failure : Exception { internal string Reason; internal Failure(string reason) { Reason = reason; } }
    internal static void Need(bool yes) { if (!yes) throw new Failure("Unsafe"); }
    internal static void Unsupported(bool yes) { if (!yes) throw new Failure("Unsupported"); }
    internal static string Hex(byte[] b) { return BitConverter.ToString(b).Replace("-", "").ToLowerInvariant(); }
    internal static byte[] Unhex(string s, int min, int max) {
        Need(s != null && s.Length % 2 == 0 && s.Length / 2 >= min && s.Length / 2 <= max && Regex.IsMatch(s, "\\A[0-9a-f]*\\z"));
        byte[] b = new byte[s.Length / 2]; for (int i = 0; i < b.Length; ++i) b[i] = byte.Parse(s.Substring(2*i, 2), NumberStyles.HexNumber, CultureInfo.InvariantCulture); return b;
    }
    internal static ulong U(string s) { ulong u; Need(Regex.IsMatch(s, "\\A(0|[1-9][0-9]*)\\z") && ulong.TryParse(s, NumberStyles.None, CultureInfo.InvariantCulture, out u)); return ulong.Parse(s, CultureInfo.InvariantCulture); }
    internal static long S(string s) { long n; Need(Regex.IsMatch(s,"\\A(0|-?[1-9][0-9]*)\\z") && long.TryParse(s, NumberStyles.AllowLeadingSign, CultureInfo.InvariantCulture,out n)); return long.Parse(s,CultureInfo.InvariantCulture); }
    internal static string Dec(ulong n) { return n.ToString(CultureInfo.InvariantCulture); }
    internal static string Signed(long n) { return n.ToString(CultureInfo.InvariantCulture); }
    internal static string Digest(byte[] b) { using (SHA256 h = SHA256.Create()) return Hex(h.ComputeHash(b)); }
    internal static string Hash(string s) { Unhex(s,32,32); return s; }
    internal static bool Equal(byte[] a, byte[] b) { return Hex(a) == Hex(b); }
    internal static byte[] Slice(byte[] b, int at, int n) { Need(at >= 0 && n >= 0 && at <= b.Length-n); byte[] r=new byte[n]; Buffer.BlockCopy(b,at,r,0,n); return r; }
    internal static void Ascii(string text) { foreach(char c in text) Need(c<=127 && c!='\r' && c!='\0'); }
    internal static string[] Components(string profile) {
        Need(profile != null && profile.Length >= 4 && profile.Length <= 128 && profile[0]>='A' && profile[0]<='Z' && profile[1]==':' && profile[2]=='\\');
        Need(Utf16.GetString(Utf16.GetBytes(profile)) == profile);
        string[] parts=profile.Substring(3).Split('\\');
        foreach(string p in parts) {
            Need(p.Length>0 && p.Length<=255 && p!="." && p!=".." && !p.EndsWith(".",StringComparison.Ordinal) && !p.EndsWith(" ",StringComparison.Ordinal));
            foreach(char c in p) Need(!char.IsControl(c) && "/:<>\"|?*".IndexOf(c)<0);
        }
        Need(parts.Length+3<=12); return parts;
    }
    internal static bool SidBytes(byte[] b) { return b!=null && b.Length>=8 && b.Length<=68 && b[0]==1 && b[1]<=15 && b.Length==8+4*b[1]; }
    internal static byte[] SidAt(byte[] b,int at,int end) {
        Need(at>=0 && at<=end-8 && end<=b.Length); int n=8+4*b[at+1]; Need(n<=68 && at<=end-n);
        byte[] sid=Slice(b,at,n); Need(SidBytes(sid));
        GCHandle pin=GCHandle.Alloc(sid,GCHandleType.Pinned); try { Need(IsValidSid(pin.AddrOfPinnedObject()) && GetLengthSid(pin.AddrOfPinnedObject())==n); } finally {pin.Free();} return sid;
    }
    internal static byte[] KnownSid(params uint[] sub) {
        byte[] b=new byte[8+4*sub.Length]; b[0]=1; b[1]=(byte)sub.Length; b[7]=5;
        for(int i=0;i<sub.Length;i++) Buffer.BlockCopy(BitConverter.GetBytes(sub[i]),0,b,8+4*i,4); return b;
    }
    internal static bool Trusted(byte[] sid,byte[] account,bool ancestor) {
        return Equal(sid,account) || Equal(sid,KnownSid(18)) || Equal(sid,KnownSid(32,544)) || (ancestor && Equal(sid,KnownSid(80,956008885,3418522649,1831038044,1853292631,2271478464)));
    }
    internal static string SecurityBytes(byte[] descriptor,uint actual,byte[] account,bool ancestor,bool validate = true) {
        Need(actual>=20 && actual<=16384 && descriptor.Length>=actual && descriptor[0]==1);
        ushort control=BitConverter.ToUInt16(descriptor,2); Need((control&0x8004)==0x8004);
        uint ownerOffset=BitConverter.ToUInt32(descriptor,4), aclOffset=BitConverter.ToUInt32(descriptor,16);
        Need(ownerOffset>=20 && ownerOffset<actual && aclOffset>=20 && aclOffset<=actual-8);
        byte[] owner=SidAt(descriptor,(int)ownerOffset,(int)actual); if(validate)Need(ancestor ? Trusted(owner,account,true) : Equal(owner,account));
        int a=(int)aclOffset; byte rev=descriptor[a]; Need(rev==2 || rev==4);
        ushort size=BitConverter.ToUInt16(descriptor,a+2), count=BitConverter.ToUInt16(descriptor,a+4);
        Need(size>=8 && size<=actual && a<=actual-size && count<=128); int end=a+size, pos=a+8;
        using(MemoryStream m=new MemoryStream()) using(BinaryWriter w=new BinaryWriter(m,Encoding.ASCII)) {
            w.Write(Encoding.ASCII.GetBytes("KTU2-winsec\0")); w.Write((uint)(control&0x1504)); w.Write((uint)owner.Length);w.Write(owner);w.Write(rev);w.Write((uint)count);
            for(int i=0;i<count;i++) {
                Need(pos<=end-8); byte type=descriptor[pos], flags=descriptor[pos+1]; ushort n=BitConverter.ToUInt16(descriptor,pos+2); uint raw=BitConverter.ToUInt32(descriptor,pos+4);
                Unsupported((type==0 || type==1) && (flags&~31)==0); Need(n>=16 && pos<=end-n);
                byte[] sid=SidAt(descriptor,pos+8,pos+n); Need(8+sid.Length==n);
                if(validate && type==0 && (flags&8)==0 && !Trusted(sid,account,ancestor)) {
                    uint mapped=raw; GENERIC_MAPPING mapping=new GENERIC_MAPPING();mapping.Read=0x120089;mapping.Write=0x120116;mapping.Execute=0x1200a0;mapping.All=0x1f01ff;
                    MapGenericMask(ref mapped,ref mapping);
                    uint dangerous=Delete|0x40000|0x80000|0x40|0x100|0x10; if(!ancestor) dangerous|=2|4;
                    Need((mapped&dangerous)==0);
                }
                w.Write(type);w.Write(flags);w.Write(raw);w.Write((uint)sid.Length);w.Write(sid); pos+=n;
            }
            Need(pos<=end);w.Flush();return Digest(m.ToArray());
        }
    }
    internal static string Security(Owned h,byte[] account,bool ancestor,bool validate = true) {
        byte[] b=new byte[16384];uint n; if(!GetKernelObjectSecurity(h.Value,5,b,(uint)b.Length,out n)) throw new Failure(Marshal.GetLastWin32Error()==122 ? "Unsupported" : "Io");
        return SecurityBytes(b,n,account,ancestor,validate);
    }
    internal class Ops {
        internal bool Uncertain;
        internal virtual void Hit(string label,Chain chain) {}
        internal virtual int WriteLimit(int count) { return count; }
        internal void Close(Owned h,string label) {
            IntPtr raw=h.Value;h.Value=IntPtr.Zero;if(raw==IntPtr.Zero)return;
            bool ok=CloseHandle(raw);try {Hit("close:"+label,null);}catch {Uncertain=true;throw;}
            if(!ok){Uncertain=true;throw new Failure("Io");}
        }
    }
    internal sealed class Owned {
        internal IntPtr Value;internal Ops Ops;
        internal Owned(IntPtr value,Ops ops){Need(value!=IntPtr.Zero && value!=new IntPtr(-1));Value=value;Ops=ops;}
        internal void Close(string label){Ops.Close(this,label);}
    }
    internal class Account {
        internal byte[] Sid,Bash;internal string Profile;
        internal string Key { get { return Hex(Sid)+"|"+Hex(Utf16.GetBytes(Profile))+"|"+Hex(Bash); } }
    }
    internal static Account ValidateAccount(byte[] sid,string profile,string env,string ps,string bashhex,string nativehex) {
        Need(SidBytes(sid));Components(profile);
        byte[] bash=Unhex(bashhex,1,256), native=Unhex(nativehex,1,512);string bashText=Utf8.GetString(bash),nativeText=Utf8.GetString(native);
        Need(Equal(Utf8.GetBytes(bashText),bash) && Equal(Utf8.GetBytes(nativeText),native));
        foreach(char c in bashText) Need(!char.IsControl(c));
        Need(profile==env && profile==ps && profile==nativeText && Utf16.GetString(Utf16.GetBytes(nativeText))==nativeText);
        Account result=new Account();result.Sid=sid;result.Profile=profile;result.Bash=bash;return result;
    }
    internal static Account ProductionAccount(Ops ops,string bash,string native,string ps,string env) {
        Unsupported(Environment.OSVersion.Platform==PlatformID.Win32NT && IntPtr.Size==8);
        ushort process,nativeMachine;Unsupported(IsWow64Process2(GetCurrentProcess(),out process,out nativeMachine) && process==0 && nativeMachine==0x8664);
        IntPtr thread;
        if(OpenThreadToken(GetCurrentThread(),8,true,out thread)) {new Owned(thread,ops).Close("thread-token");throw new Failure("Account");}
        Unsupported(Marshal.GetLastWin32Error()==1008);
        IntPtr raw; if(!OpenProcessToken(GetCurrentProcess(),8,out raw))throw new Failure("Unsupported");Owned token=new Owned(raw,ops);
        try {
            byte[] b=new byte[256];uint n;GCHandle pin=GCHandle.Alloc(b,GCHandleType.Pinned);byte[] sid;
            try {
                if(!GetTokenInformation(token.Value,1,b,(uint)b.Length,out n))throw new Failure("Unsupported");Need(n>=16 && n<=b.Length);
                long baseAddress=pin.AddrOfPinnedObject().ToInt64(), pointer=Marshal.ReadIntPtr(pin.AddrOfPinnedObject()).ToInt64();
                long at=pointer-baseAddress;Need(at>=0 && at<=n-8);sid=SidAt(b,(int)at,(int)n);
            } finally {pin.Free();}
            char[] profile=new char[129];uint chars=(uint)profile.Length;
            if(!GetUserProfileDirectoryW(token.Value,profile,ref chars))throw new Failure("Unsupported");
            int end=Array.IndexOf(profile,'\0');Need(end>=1 && end<=128 && chars>0 && chars<=129);
            Account account;try{account=ValidateAccount(sid,new string(profile,0,end),env,ps,bash,native);}catch(Failure){throw new Failure("Account");}catch(DecoderFallbackException){throw new Failure("Account");}catch(EncoderFallbackException){throw new Failure("Account");}token.Close("token");return account;
        } catch {if(token.Value!=IntPtr.Zero)try{token.Close("token");}catch{} throw;}
    }
    internal static string Boot() {
        Unsupported(IntPtr.Size==8 && Marshal.SizeOf(typeof(BOOT))==32 && Marshal.OffsetOf(typeof(BOOT),"Firmware").ToInt32()==16 && Marshal.OffsetOf(typeof(BOOT),"Flags").ToInt32()==24);
        IntPtr module=GetModuleHandleW("ntdll.dll");Unsupported(module!=IntPtr.Zero);IntPtr address=GetProcAddress(module,"NtQuerySystemInformation");Unsupported(address!=IntPtr.Zero);
        QueryBoot query=(QueryBoot)Marshal.GetDelegateForFunctionPointer(address,typeof(QueryBoot));byte[] b=new byte[32];uint length;
        int status=query(90,b,(uint)b.Length,out length);Unsupported(status==0 && length==32 && (BitConverter.ToUInt32(b,16)==1 || BitConverter.ToUInt32(b,16)==2));
        string boot=Hex(Slice(b,0,16));Unsupported(boot!=new string('0',32));return boot;
    }
    internal class Lease {
        internal string BootId;internal ulong Tick,End;
        internal static Lease Start(ulong ttl,long headerQ,long frequency) {
            Unsupported(frequency>0 && frequency==Stopwatch.Frequency);
            string boot=Boot();ulong tick=GetTickCount64();long q=Stopwatch.GetTimestamp();Unsupported(q>=headerQ);
            ulong elapsed=checked(((ulong)(q-headerQ)*1000+(ulong)frequency-1)/(ulong)frequency);
            if(elapsed>=ttl)throw new Failure("Expired");Lease l=new Lease();l.BootId=boot;l.Tick=tick;l.End=checked(tick+ttl-elapsed);return l;
        }
        internal void Check() { ulong now=GetTickCount64();if(Boot()!=BootId || now<Tick || now>=End)throw new Failure("Expired"); }
    }
    internal class State {
        internal ulong Volume,Size;internal byte[] Id;internal uint Attrs,Links;internal long WriteTime,ChangeTime;internal bool Directory,Pending;internal string Sec,Hash;
        internal string Identity {get{return Dec(Volume)+","+Hex(Id);}}
        internal string Parent {get{return Identity+","+Dec(Attrs)+","+Sec;}}
        internal string Leaf {get{return "P,"+Identity+","+Dec(Size)+","+Dec(Attrs)+","+Signed(WriteTime)+","+Signed(ChangeTime)+","+Dec(Links)+","+Sec+","+Hash;}}
        internal bool Same(State b,bool rename) { return b!=null && Identity==b.Identity && Size==b.Size && Attrs==b.Attrs && WriteTime==b.WriteTime && (rename || ChangeTime==b.ChangeTime) && Links==b.Links && Directory==b.Directory && !Pending && !b.Pending && Sec==b.Sec && Hash==b.Hash; }
    }
    internal static State Stat(Owned h,byte[] account,bool ancestor,bool directory,bool validate = true) {
        Need(h.Value!=IntPtr.Zero && GetFileType(h.Value)==1);
        byte[] id=new byte[24],basic=new byte[40],standard=new byte[24];
        if(!GetFileInformationByHandleEx(h.Value,18,id,(uint)id.Length) || !GetFileInformationByHandleEx(h.Value,0,basic,(uint)basic.Length) || !GetFileInformationByHandleEx(h.Value,1,standard,(uint)standard.Length))throw new Failure("Io");
        State s=new State();s.Volume=BitConverter.ToUInt64(id,0);s.Id=Slice(id,8,16);s.Attrs=BitConverter.ToUInt32(basic,32);s.WriteTime=BitConverter.ToInt64(basic,16);s.ChangeTime=BitConverter.ToInt64(basic,24);
        long size=BitConverter.ToInt64(standard,8);Need(size>=0);s.Size=(ulong)size;s.Links=BitConverter.ToUInt32(standard,16);s.Pending=standard[20]!=0;s.Directory=standard[21]!=0;
        Need(!s.Pending && (s.Attrs&UnsafeAttrs)==0 && s.Directory==directory && ((s.Attrs&16)!=0)==directory);
        if(!directory)Need(s.Links==1 && s.Size<=Limit && (s.Attrs&1)==0);
        s.Sec=Security(h,account,ancestor,validate);return s;
    }
    internal static void Component(string name) {
        Need(name.Length>0 && name.Length<=255 && name!="." && name!=".." && !name.EndsWith(".",StringComparison.Ordinal) && !name.EndsWith(" ",StringComparison.Ordinal));
        foreach(char c in name)Need(!char.IsControl(c) && "/\\:<>\"|?*".IndexOf(c)<0);
    }
    internal static Owned Open(Owned parent,string name,uint access,uint share,uint disposition,bool directory,bool named,byte[] owner,Ops ops) {
        Component(name);byte[] wide=Utf16.GetBytes(name);IntPtr text=Marshal.AllocHGlobal(wide.Length),unicode=Marshal.AllocHGlobal(16),sd=IntPtr.Zero;GCHandle sidPin=new GCHandle();
        try {
            Marshal.Copy(wide,0,text,wide.Length);UNICODE u=new UNICODE();u.Length=(ushort)wide.Length;u.MaximumLength=u.Length;u.Buffer=text;Marshal.StructureToPtr(u,unicode,false);
            ATTRIBUTES attributes=new ATTRIBUTES();attributes.Length=48;attributes.Root=parent.Value;attributes.Name=unicode;attributes.Flags=0x40|0x1000;
            if(owner!=null) {
                sd=Marshal.AllocHGlobal(40);for(int i=0;i<40;i++)Marshal.WriteByte(sd,i,0);Unsupported(InitializeSecurityDescriptor(sd,1));
                sidPin=GCHandle.Alloc(owner,GCHandleType.Pinned);Unsupported(SetSecurityDescriptorOwner(sd,sidPin.AddrOfPinnedObject(),false));attributes.Security=sd;
            }
            IO_STATUS io=new IO_STATUS();IntPtr raw;uint options=disposition==2 && directory ? 1|Synchronous : NoReparse|NoRecall|Synchronous|(directory||named?0u:0x40u);
            int status=NtCreateFile(out raw,access|Sync,ref attributes,out io,IntPtr.Zero,0x80,share,disposition,options,IntPtr.Zero,0);
            if(status==unchecked((int)0xc0000034) && disposition==1) {if(raw!=IntPtr.Zero && raw!=new IntPtr(-1)){ops.Uncertain=true;new Owned(raw,ops).Close("unexpected-notfound");throw new Failure("Unsupported");}return null;}
            if(status!=0) {if(raw!=IntPtr.Zero && raw!=new IntPtr(-1)){Owned unexpected=new Owned(raw,ops);unexpected.Close("unexpected-open");}throw new Failure("Io");}
            Owned h=new Owned(raw,ops);if(io.Status!=IntPtr.Zero || io.Information.ToUInt64()!=(disposition==2?2ul:1ul)){ops.Uncertain=true;h.Close("invalid-open");throw new Failure("Unsupported");}return h;
        } finally {if(sidPin.IsAllocated)sidPin.Free();if(sd!=IntPtr.Zero)Marshal.FreeHGlobal(sd);Marshal.FreeHGlobal(unicode);Marshal.FreeHGlobal(text);}
    }
    internal class Manifest {
        internal string Root,RootIdentity;internal int RootIndex;internal string[] Identities,Security;internal byte[] Sid;
    }
    internal class Chain {
        internal List<Owned> Handles=new List<Owned>();internal List<string> Names=new List<string>(),Parents=new List<string>();internal Account Account;internal Ops Ops;internal Manifest Manifest;
        internal Owned Bin {get{return Handles[Handles.Count-1];}}
        internal void Verify() {
            ulong volume=0;
            for(int i=0;i<Handles.Count;i++) {
                bool ancestor=i<Handles.Count-3;State held=Stat(Handles[i],Account.Sid,ancestor,true,Manifest==null || i>=Manifest.RootIndex);if(i==0)volume=held.Volume;Need(held.Volume==volume && held.Parent==Parents[i]);
                if(Manifest!=null){if(i==Manifest.RootIndex)Need(held.Identity==Manifest.RootIdentity);if(Manifest.Identities.Length>0)Need(held.Identity==Manifest.Identities[i] && held.Sec==Manifest.Security[i]);}
                if(i>0){Owned named=Open(Handles[i-1],Names[i-1],Attr|Control,7,1,true,true,null,Ops);Need(named!=null);try{State state=Stat(named,Account.Sid,ancestor,true,Manifest==null || i>=Manifest.RootIndex);Need(state.Parent==held.Parent);}finally{named.Close("parent-name");}}
            }
        }
        internal string ParentRecord {get{return string.Join(";",Parents.ToArray());}}
        internal void CloseAll() {for(int i=Handles.Count-1;i>=0;i--)if(Handles[i].Value!=IntPtr.Zero)try{Handles[i].Close(i==Handles.Count-1?"bin":"parent");}catch{Ops.Uncertain=true;}}
    }
    internal static Chain OpenChain(Account account,Ops ops,Manifest manifest) {
        string[] parts=Components(account.Profile);Chain c=new Chain();c.Account=account;c.Ops=ops;c.Manifest=manifest;
        try {
            string root=account.Profile.Substring(0,3);Unsupported(GetDriveTypeW(root)==3);
            IntPtr raw=CreateFileW(root,Attr|Control|0x20|Sync,3,IntPtr.Zero,3,0x02000000|0x00200000|0x00100000,IntPtr.Zero);Owned drive=new Owned(raw,ops);c.Handles.Add(drive);
            StringBuilder fs=new StringBuilder(32);if(!GetVolumeInformationByHandleW(raw,IntPtr.Zero,0,IntPtr.Zero,IntPtr.Zero,IntPtr.Zero,fs,32))throw new Failure("Io");Unsupported(fs.ToString()=="NTFS");
            List<string> all=new List<string>(parts);all.Add(".katala-tune");all.Add("bin");c.Names=all;
            for(int i=0;i<=all.Count;i++) {
                Owned h=c.Handles[i];bool ancestor=i<all.Count-2;State state=Stat(h,account.Sid,ancestor,true,manifest==null || i>=manifest.RootIndex);
                if(i>0)Need(state.Volume==U(c.Parents[0].Split(',')[0]));c.Parents.Add(state.Parent);
                if(i<all.Count){Owned next=Open(h,all[i],Attr|Control|0x20,3,1,true,false,null,ops);Need(next!=null);c.Handles.Add(next);}
            }
            if(manifest!=null){Need(Equal(manifest.Sid,account.Sid) && manifest.Identities.Length==manifest.Security.Length && (manifest.Identities.Length==0 || manifest.Identities.Length==c.Handles.Count));Need(manifest.RootIndex==Components(manifest.Root).Length && account.Profile==manifest.Root+"\\home");}
            c.Verify();ops.Hit("opened",c);return c;
        } catch {c.CloseAll();throw;}
    }
    internal static State Named(Chain c,string name,bool directory) {
        Owned h=Open(c.Bin,name,Attr|Control,7,1,directory,true,null,c.Ops);if(h==null)return null;
        try{return Stat(h,c.Account.Sid,false,directory);}finally{h.Close("named");}
    }
    internal static State ReadLeaf(Chain c,string name) {
        State named=Named(c,name,false);if(named==null)return null;
        Owned h=Open(c.Bin,name,Read|Control,1,1,false,false,null,c.Ops);Need(h!=null);
        try {
            State before=Stat(h,c.Account.Sid,false,false);Need(before.Same(named,false));
            using(SHA256 hash=SHA256.Create()) {
                byte[] b=new byte[65536];ulong total=0;uint n;
                while(true){if(!ReadFile(h.Value,b,(uint)b.Length,out n,IntPtr.Zero))throw new Failure("Io");if(n==0)break;Need(n<=b.Length);total=checked(total+n);Need(total<=Limit);hash.TransformBlock(b,0,(int)n,b,0);}
                hash.TransformFinalBlock(new byte[0],0,0);before.Hash=Hex(hash.Hash);Need(total==before.Size);
            }
            State after=Stat(h,c.Account.Sid,false,false), again=Named(c,name,false);after.Hash=before.Hash;if(again!=null)again.Hash=before.Hash;Need(before.Same(after,false) && before.Same(again,false));
            h.Close("read");return before;
        } catch {if(h.Value!=IntPtr.Zero)try{h.Close("read");}catch{}throw;}
    }
    internal static bool Same(State a,State b,bool rename) {return a==null ? b==null : a.Same(b,rename);}
    internal static string Leaf(State s) {return s==null?"M":s.Leaf;}
    internal class Observation {
        internal string Record,AccountKey,Parents;internal Lease Lease;internal State Before;
    }
    internal static Observation ParseObservation(string record) {
        Need(record.Length<=3072);Ascii(record);string[] f=record.Split('|');Need(f.Length==11 && f[0]=="w1" && f[4]=="windows" && f[5]=="x64");
        byte[] sid=Unhex(f[1],8,68);Need(SidBytes(sid));string profile=Utf16.GetString(Unhex(f[2],2,256));string[] parts=Components(profile);
        byte[] bash=Unhex(f[3],1,256);foreach(char ch in Utf8.GetString(bash))Need(!char.IsControl(ch));
        Lease lease=new Lease();lease.BootId=Hex(Unhex(f[6],16,16));Need(lease.BootId!=new string('0',32));lease.Tick=U(f[7]);lease.End=U(f[8]);Need(lease.End>lease.Tick && lease.End-lease.Tick<=120000);
        string[] parents=f[9].Split(';');Need(parents.Length==parts.Length+3 && parents.Length<=12);
        foreach(string parent in parents){string[] p=parent.Split(',');Need(p.Length==4);U(p[0]);Unhex(p[1],16,16);Need(U(p[2])<=uint.MaxValue);Hash(p[3]);}
        State old=null;if(f[10]!="M") {
            string[] p=f[10].Split(',');Need(p.Length==10 && p[0]=="P" && p[7]=="1");old=new State();old.Volume=U(p[1]);old.Id=Unhex(p[2],16,16);old.Size=U(p[3]);Need(old.Size>0 && old.Size<=Limit);ulong attrs=U(p[4]);Need(attrs<=uint.MaxValue);old.Attrs=(uint)attrs;old.WriteTime=S(p[5]);old.ChangeTime=S(p[6]);old.Links=1;old.Sec=Hash(p[8]);old.Hash=Hash(p[9]);Need(old.Leaf==f[10]);
        }
        Observation o=new Observation();o.Record=record;o.AccountKey=f[1]+"|"+f[2]+"|"+f[3];o.Parents=f[9];o.Lease=lease;o.Before=old;return o;
    }
    internal class Header {
        internal string Token,Version,Manifest,Hash,SourceHash,Context,Mode,RawObservation;internal ulong Size,SourceSize,Ttl;internal Observation Expected;
        internal static Header Parse(byte[] raw,ulong body) {
            Need(raw.Length>0 && raw.Length<=16384);foreach(byte b in raw)Need(b<128 && b!=13 && b!=0);
            string[] lines=Encoding.ASCII.GetString(raw).Split('\n');Need(lines.Length==13 && lines[12]=="");
            string[] names={"token","version","target","manifest","size","sha256","context","mode","source-size","source-sha256","ttl-ms","observation"};string[] f=new string[12];
            for(int i=0;i<12;i++){Need(lines[i].StartsWith(names[i]+"=",StringComparison.Ordinal));f[i]=lines[i].Substring(names[i].Length+1);}
            Need(Regex.IsMatch(f[0],"\\A[A-Za-z0-9_-]{1,64}\\z") && Regex.IsMatch(f[1],"\\A[A-Za-z0-9._-]{1,128}\\z"));Unsupported(f[2]=="x86_64-pc-windows-msvc");
            Header h=new Header();h.Token=f[0];h.Version=f[1];h.Manifest=Ktu2.Hash(f[3]);h.Size=U(f[4]);h.Hash=Ktu2.Hash(f[5]);h.Context=Ktu2.Hash(f[6]);h.Mode=f[7];h.SourceSize=U(f[8]);h.SourceHash=Ktu2.Hash(f[9]);h.Ttl=U(f[10]);h.RawObservation=f[11];
            Need(h.Size==body && body<=Limit && h.SourceSize>0 && h.SourceSize<=Limit && h.Ttl>0 && h.Ttl<=120000);
            if(h.Mode=="inspect")Need(body==0 && h.Hash==Digest(new byte[0]) && h.RawObservation=="-");
            else {Need(h.Mode=="place" && body==h.SourceSize && h.Hash==h.SourceHash);h.Expected=ParseObservation(h.RawObservation);}return h;
        }
    }
    internal static string InspectAt(Chain chain,Account account,Lease lease) {
        chain.Verify();State leaf=ReadLeaf(chain,Target);chain.Verify();lease.Check();
        string record="w1|"+account.Key+"|windows|x64|"+lease.BootId+"|"+Dec(lease.Tick)+"|"+Dec(lease.End)+"|"+chain.ParentRecord+"|"+Leaf(leaf);
        ParseObservation(record);return record;
    }
    internal static void Drain(Stream input) {byte[] b=new byte[65536];while(input.Read(b,0,b.Length)>0){}}
    internal static void Body(Chain c,Stream input,Header h,Owned stage) {
        using(SHA256 hash=SHA256.Create()) {
            byte[] b=new byte[65536];ulong left=h.Size;
            while(left>0){int n=input.Read(b,0,(int)Math.Min((ulong)b.Length,left));Need(n>0);hash.TransformBlock(b,0,n,b,0);left-=(ulong)n;
                int offset=0;while(stage!=null && offset<n){int wanted=c.Ops.WriteLimit(n-offset);Need(wanted>0 && wanted<=n-offset);byte[] part=offset==0?b:Slice(b,offset,n-offset);uint written;
                    if(!WriteFile(stage.Value,part,(uint)wanted,out written,IntPtr.Zero))throw new Failure("Io");Need(written>0 && written<=wanted);offset+=(int)written;c.Ops.Hit("write",c);}
            }
            hash.TransformFinalBlock(new byte[0],0,0);Need(Hex(hash.Hash)==h.SourceHash && input.ReadByte()==-1);
        }
    }
    internal static Owned Mutation(Chain c,string name,State expected) {
        Owned h=Open(c.Bin,name,Read|Control|Delete,5,1,false,false,null,c.Ops);Need(h!=null);
        try{State s=Stat(h,c.Account.Sid,false,false);s.Hash=expected.Hash;State named=Named(c,name,false);if(named!=null)named.Hash=expected.Hash;Need(expected.Same(s,false) && expected.Same(named,false));
            using(SHA256 hash=SHA256.Create()){byte[] b=new byte[65536];ulong total=0;uint n;while(true){if(!ReadFile(h.Value,b,(uint)b.Length,out n,IntPtr.Zero))throw new Failure("Io");if(n==0)break;Need(n<=b.Length);total=checked(total+n);Need(total<=Limit);hash.TransformBlock(b,0,(int)n,b,0);}hash.TransformFinalBlock(new byte[0],0,0);Need(total==expected.Size && Hex(hash.Hash)==expected.Hash);}
            State after=Stat(h,c.Account.Sid,false,false);after.Hash=expected.Hash;State finalName=Named(c,name,false);if(finalName!=null)finalName.Hash=expected.Hash;Need(expected.Same(after,false) && expected.Same(finalName,false));return h;
        }catch{if(h.Value!=IntPtr.Zero)try{h.Close("mutation");}catch{}throw;}
    }
    internal static void Rename(Chain c,string from,string to,State expected) {
        Component(to);Owned h=Mutation(c,from,expected);try{
            Unsupported(Marshal.SizeOf(typeof(RENAME))==24 && Marshal.OffsetOf(typeof(RENAME),"Root").ToInt32()==8 && Marshal.OffsetOf(typeof(RENAME),"Length").ToInt32()==16 && Marshal.OffsetOf(typeof(RENAME),"Name").ToInt32()==20);
            byte[] wide=Utf16.GetBytes(to);int size=checked(24+wide.Length);IntPtr p=Marshal.AllocHGlobal(size);
            try{for(int i=0;i<size;i++)Marshal.WriteByte(p,i,0);Marshal.WriteIntPtr(p,8,c.Bin.Value);Marshal.WriteInt32(p,16,wide.Length);Marshal.Copy(wide,0,IntPtr.Add(p,20),wide.Length);IO_STATUS io;int result=NtSetInformationFile(h.Value,out io,p,(uint)size,10);
                if(result!=0 || io.Status!=IntPtr.Zero || io.Information.ToUInt64()!=0){c.Ops.Uncertain=true;throw new Failure("Io");}
                // A later close failure must never trigger a second mutation.
                try{State moved=Stat(h,c.Account.Sid,false,false);moved.Hash=expected.Hash;Need(expected.Same(moved,true));}catch{c.Ops.Uncertain=true;throw;}
            }finally{Marshal.FreeHGlobal(p);}h.Close("mutation");
        }catch{if(h.Value!=IntPtr.Zero)try{h.Close("mutation");}catch{}throw;}
    }
    internal static bool OwnedName(Chain c,string name,State expected) {State named=Named(c,name,false);return named!=null && expected!=null && named.Identity==expected.Identity && named.Sec==expected.Sec && named.Links==1;}
    internal static void Remove(Chain c,string name,State expected) {
        Need(!c.Ops.Uncertain && OwnedName(c,name,expected));State before=ReadLeaf(c,name);Need(expected.Same(before,false));Owned h=Mutation(c,name,before);
        try {DISPOSITION d=new DISPOSITION();d.Delete=1;if(!SetFileInformationByHandle(h.Value,4,ref d,1))throw new Failure("Io");h.Close("delete");Need(Named(c,name,false)==null);}catch{if(h.Value!=IntPtr.Zero)try{h.Close("delete");}catch{}throw;}
    }
    internal static void Finish(Chain c,ref Owned lockHandle,State lockState) {
        Need(!c.Ops.Uncertain);c.Verify();
        for(int i=c.Handles.Count-2;i>=0;i--)c.Handles[i].Close("parent");
        Need(lockHandle!=null && lockHandle.Value!=IntPtr.Zero);State held=Stat(lockHandle,c.Account.Sid,false,true),named=Named(c,Lock,true);Need(named!=null && held.Parent==lockState.Parent && named.Parent==lockState.Parent);
        c.Ops.Hit("lock_disposition",c);DISPOSITION d=new DISPOSITION();d.Delete=1;if(!SetFileInformationByHandle(lockHandle.Value,4,ref d,1))throw new Failure("Io");
        lockHandle.Close("lock");Need(Named(c,Lock,true)==null);c.Bin.Close("bin");Need(!c.Ops.Uncertain);
    }
    internal class Wire {
        internal Stream Output;internal Header Header;
        internal Wire(Stream output,Header header){Output=output;Header=header;}
        internal void Emit(string eventName,string detail) {string text="KTU2 "+Header.Token+" "+Header.Context+" "+eventName+" "+Header.SourceHash+" "+detail+"\n";Ascii(text);byte[] bytes=Encoding.ASCII.GetBytes(text);Need(bytes.Length<=4096);Output.Write(bytes,0,bytes.Length);Output.Flush();}
        internal void Placement(string eventName,bool backup,bool rollback) {Emit(eventName,(Header.Expected.Before==null?"-":Header.Expected.Before.Hash)+" "+(backup?"1":"0")+" "+(rollback?"1":"0"));}
    }
    internal static int PlaceAt(Chain c,Account account,Header h,Stream input,Wire wire,Func<Account> currentAccount) {
        Observation expected=h.Expected;State old=expected.Before;Need(expected.AccountKey==account.Key);expected.Lease.Check();c.Verify();Need(c.ParentRecord==expected.Parents && Same(ReadLeaf(c,Target),old,false));
        wire.Placement("Started",false,false);Owned lockHandle=null,stageHandle=null;State lockState=null,stageState=null,backupState=null,finalState=null;
        string stage=".tune-agent-stage-"+h.Token,backup=".tune-agent-backup-"+h.Token;bool backed=false,final=false,retained=false,rollback=false;
        try {
            c.Ops.Hit("before_lock",c);lockHandle=Open(c.Bin,Lock,Attr|Control|0x20|Delete,3,2,true,false,account.Sid,c.Ops);Need(lockHandle!=null);try{lockState=Stat(lockHandle,account.Sid,false,true);State lockNamed=Named(c,Lock,true);Need(lockNamed!=null && lockNamed.Parent==lockState.Parent);}catch{c.Ops.Uncertain=true;throw;}
            c.Verify();Need(Same(ReadLeaf(c,Target),old,false));c.Ops.Hit("after_lock",c);
            if(old!=null && old.Hash==h.SourceHash) {Body(c,input,h,null);Need(currentAccount().Key==account.Key);c.Verify();Need(Same(ReadLeaf(c,Target),old,false));Finish(c,ref lockHandle,lockState);wire.Placement("Unchanged",false,false);return 0;}
            Need(Named(c,backup,false)==null);
            stageHandle=Open(c.Bin,stage,Read|Write|Control,1,2,false,false,account.Sid,c.Ops);Need(stageHandle!=null);try{stageState=Stat(stageHandle,account.Sid,false,false);Need(stageState.Size==0);}catch{c.Ops.Uncertain=true;throw;}
            Body(c,input,h,stageHandle);if(!FlushFileBuffers(stageHandle.Value))throw new Failure("Io");c.Ops.Hit("flush",c);stageHandle.Close("stage");
            State staged=ReadLeaf(c,stage);Need(staged!=null && staged.Identity==stageState.Identity && staged.Sec==stageState.Sec && staged.Size==h.Size && staged.Hash==h.SourceHash);stageState=staged;wire.Placement("Staged",false,false);
            c.Ops.Hit("before_commit",c);Need(currentAccount().Key==account.Key);c.Verify();Need(c.ParentRecord==expected.Parents && Same(ReadLeaf(c,Target),old,false) && Same(ReadLeaf(c,stage),stageState,false));
            if(old!=null){Rename(c,Target,backup,old);backed=true;backupState=ReadLeaf(c,backup);retained=Same(backupState,old,true);Need(retained);wire.Placement("BackedUp",true,false);c.Ops.Hit("after_backup",c);}
            Rename(c,stage,Target,stageState);final=true;c.Ops.Hit("after_final_rename",c);c.Verify();Need(OwnedName(c,Target,stageState));finalState=ReadLeaf(c,Target);Need(Same(finalState,stageState,true));
            if(old!=null){c.Ops.Hit("before_backup_verify",c);retained=false;retained=Same(ReadLeaf(c,backup),backupState,false);Need(retained);}
            c.Ops.Hit("final_verify",c);c.Verify();Need(OwnedName(c,Target,stageState) && Same(ReadLeaf(c,Target),finalState,false));
            if(old!=null){c.Ops.Hit("before_backup_verify",c);retained=false;retained=Same(ReadLeaf(c,backup),backupState,false);Need(retained);}
            Finish(c,ref lockHandle,lockState);wire.Placement("Placed",backed,false);return 0;
        } catch {
            if(stageHandle!=null && stageHandle.Value!=IntPtr.Zero)try{stageHandle.Close("stage");}catch{}
            if(!c.Ops.Uncertain && c.Bin.Value!=IntPtr.Zero)try {
                c.Verify();
                if(backed){retained=false;retained=backupState!=null && Same(ReadLeaf(c,backup),backupState,false);Need(retained);
                    State occupying=Named(c,Target,false);if(occupying!=null){Need(final && OwnedName(c,Target,stageState));State own=ReadLeaf(c,Target);Need(Same(own,stageState,true));Remove(c,Target,own);}
                    c.Ops.Hit("before_rollback",c);Rename(c,backup,Target,backupState);backed=false;retained=false;Need(Same(ReadLeaf(c,Target),old,true));c.Ops.Hit("after_rollback",c);rollback=true;
                }else if(final && old==null){Need(OwnedName(c,Target,stageState));State own=ReadLeaf(c,Target);Need(Same(own,stageState,true));Remove(c,Target,own);Need(Named(c,Target,false)==null);rollback=true;}
                if(stageState!=null && OwnedName(c,stage,stageState)){c.Ops.Hit("cleanup",c);State own=ReadLeaf(c,stage);Need(own.Identity==stageState.Identity && own.Sec==stageState.Sec);Remove(c,stage,own);}
                if(lockState!=null)Finish(c,ref lockHandle,lockState);else c.CloseAll();
            }catch{rollback=false;}
            try{Drain(input);}catch{rollback=false;}
            if(lockHandle!=null && lockHandle.Value!=IntPtr.Zero)try{lockHandle.Close("lock");}catch{rollback=false;}
            c.CloseAll();if(c.Ops.Uncertain)rollback=false;wire.Placement("Failed",retained,rollback);return 2;
        }
    }
    internal static int RunAt(Stream input,byte[] headerBytes,ulong body,Account account,Ops ops,Manifest manifest,long q,long frequency,Stream output,Func<Account> currentAccount) {
        Header h=null;Chain chain=null;Wire wire=null;int result=2;string reason="Io",record=null;
        try {
            h=Header.Parse(headerBytes,body);wire=new Wire(output,h);if(account==null)account=currentAccount();
            Lease lease=Lease.Start(h.Ttl,q,frequency);chain=OpenChain(account,ops,manifest);
            if(h.Mode=="inspect"){Need(input.ReadByte()==-1);record=InspectAt(chain,account,lease);chain.CloseAll();Need(!ops.Uncertain);lease.Check();wire.Emit("Observed",record);result=0;}
            else result=PlaceAt(chain,account,h,input,wire,currentAccount);
        }catch(Failure failure){reason=failure.Reason;}
        catch(EntryPointNotFoundException){reason="Unsupported";}
        catch(DllNotFoundException){reason="Unsupported";}
        catch(OverflowException){reason="Unsupported";}
        catch {reason="Io";}
        finally {try{Drain(input);}catch{result=2;}if(chain!=null)chain.CloseAll();if(ops.Uncertain)result=2;}
        if(result!=0 && h!=null && h.Mode=="inspect" && record==null && wire!=null)try{wire.Emit("InspectFailed",reason);}catch{}
        return result;
    }
    public static int Run(Stream input,byte[] headerBytes,long body,string bash,string native,string ps,string env,long q,long frequency) {
        Ops ops=new Ops();Stream output=null;
        try {Header.Parse(headerBytes,checked((ulong)body));output=Console.OpenStandardOutput();Account account=null;
            int result=RunAt(input,headerBytes,checked((ulong)body),account,ops,null,q,frequency,output,delegate{return ProductionAccount(ops,bash,native,ps,env);});output.Flush();Stream consumed=output;output=null;consumed.Dispose();return result;
        } catch {try{Drain(input);}catch{}if(output!=null)try{output.Dispose();}catch{}return 2;}
    }
    [StructLayout(LayoutKind.Sequential)] internal struct BOOT {internal Guid Id;internal uint Firmware;internal ulong Flags;}
    [StructLayout(LayoutKind.Sequential)] internal struct RENAME {internal byte Replace;internal IntPtr Root;internal uint Length;internal ushort Name;}
    [StructLayout(LayoutKind.Sequential)] internal struct UNICODE {internal ushort Length,MaximumLength;internal IntPtr Buffer;}
    [StructLayout(LayoutKind.Sequential)] internal struct ATTRIBUTES {internal uint Length;internal IntPtr Root,Name;internal uint Flags;internal IntPtr Security,Quality;}
    [StructLayout(LayoutKind.Sequential)] internal struct IO_STATUS {internal IntPtr Status;internal UIntPtr Information;}
    [StructLayout(LayoutKind.Sequential)] internal struct GENERIC_MAPPING {internal uint Read,Write,Execute,All;}
    [StructLayout(LayoutKind.Sequential,Pack=1)] internal struct DISPOSITION {internal byte Delete;}
    [UnmanagedFunctionPointer(CallingConvention.StdCall)] internal delegate int QueryBoot(int kind,[Out] byte[] buffer,uint length,out uint returned);
    [DllImport("ntdll.dll")] internal static extern int NtCreateFile(out IntPtr file,uint access,ref ATTRIBUTES attributes,out IO_STATUS status,IntPtr allocation,uint flags,uint share,uint disposition,uint options,IntPtr ea,uint eaLength);
    [DllImport("ntdll.dll")] internal static extern int NtSetInformationFile(IntPtr file,out IO_STATUS status,IntPtr info,uint length,int kind);
    [DllImport("kernel32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool CloseHandle(IntPtr h);
    [DllImport("kernel32.dll")] internal static extern IntPtr GetCurrentProcess();
    [DllImport("kernel32.dll")] internal static extern IntPtr GetCurrentThread();
    [DllImport("kernel32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool IsWow64Process2(IntPtr process,out ushort processMachine,out ushort nativeMachine);
    [DllImport("advapi32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool OpenProcessToken(IntPtr process,uint access,out IntPtr token);
    [DllImport("advapi32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool OpenThreadToken(IntPtr thread,uint access,[MarshalAs(UnmanagedType.Bool)]bool self,out IntPtr token);
    [DllImport("advapi32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool GetTokenInformation(IntPtr token,int kind,[Out]byte[] buffer,uint length,out uint returned);
    [DllImport("userenv.dll",CharSet=CharSet.Unicode,SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool GetUserProfileDirectoryW(IntPtr token,[Out]char[] profile,ref uint length);
    [DllImport("advapi32.dll")] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool IsValidSid(IntPtr sid);
    [DllImport("advapi32.dll")] internal static extern uint GetLengthSid(IntPtr sid);
    [DllImport("advapi32.dll")] internal static extern void MapGenericMask(ref uint mask,ref GENERIC_MAPPING mapping);
    [DllImport("advapi32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool GetKernelObjectSecurity(IntPtr h,uint requested,[Out]byte[] buffer,uint length,out uint returned);
    [DllImport("advapi32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool InitializeSecurityDescriptor(IntPtr descriptor,uint revision);
    [DllImport("advapi32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool SetSecurityDescriptorOwner(IntPtr descriptor,IntPtr owner,[MarshalAs(UnmanagedType.Bool)]bool defaulted);
    [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] internal static extern IntPtr GetModuleHandleW(string name);
    [DllImport("kernel32.dll",CharSet=CharSet.Ansi,ExactSpelling=true,SetLastError=true)] internal static extern IntPtr GetProcAddress(IntPtr module,string name);
    [DllImport("kernel32.dll")] internal static extern ulong GetTickCount64();
    [DllImport("kernel32.dll",CharSet=CharSet.Unicode)] internal static extern uint GetDriveTypeW(string root);
    [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] internal static extern IntPtr CreateFileW(string path,uint access,uint share,IntPtr security,uint disposition,uint flags,IntPtr template);
    [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool GetVolumeInformationByHandleW(IntPtr h,IntPtr name,uint size,IntPtr serial,IntPtr max,IntPtr flags,StringBuilder fs,uint fsSize);
    [DllImport("kernel32.dll",SetLastError=true)] internal static extern uint GetFileType(IntPtr h);
    [DllImport("kernel32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool GetFileInformationByHandleEx(IntPtr h,int kind,[Out]byte[] buffer,uint length);
    [DllImport("kernel32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool ReadFile(IntPtr h,[Out]byte[] buffer,uint length,out uint read,IntPtr overlapped);
    [DllImport("kernel32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool WriteFile(IntPtr h,byte[] buffer,uint length,out uint written,IntPtr overlapped);
    [DllImport("kernel32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool FlushFileBuffers(IntPtr h);
    [DllImport("kernel32.dll",SetLastError=true)] [return:MarshalAs(UnmanagedType.Bool)] internal static extern bool SetFileInformationByHandle(IntPtr h,int kind,ref DISPOSITION info,uint length);
}
"@
function Reject-Ktu2Unsupported($stream, $header, $body) {
    # Compiler/policy refusal cannot call the C# parser. This finite, inspect-only
    # grammar emits no placement receipt and never opens a filesystem object.
    $valid = $body -eq 0 -and $header.Length -le 16384
    foreach ($byte in $header) { if ($byte -gt 127 -or $byte -eq 13 -or $byte -eq 0) { $valid = $false } }
    $text = [Text.Encoding]::ASCII.GetString($header)
    $pattern = "\Atoken=([A-Za-z0-9_-]{1,64})`nversion=[A-Za-z0-9._-]{1,128}`ntarget=x86_64-pc-windows-msvc`nmanifest=[0-9a-f]{64}`nsize=0`nsha256=e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855`ncontext=([0-9a-f]{64})`nmode=inspect`nsource-size=([1-9][0-9]{0,8})`nsource-sha256=([0-9a-f]{64})`nttl-ms=([1-9][0-9]{0,5})`nobservation=-`n\z"
    $valid = $valid -and ($text -cmatch $pattern)
    if ($valid) { $valid = [uint64]$Matches[3] -le 268435456 -and [uint64]$Matches[5] -le 120000 }
    $buffer = New-Object byte[] 65536
    while ($stream.Read($buffer, 0, $buffer.Length) -gt 0) { }
    if ($valid) {
        $line = "KTU2 " + $Matches[1] + " " + $Matches[2] + " InspectFailed " + $Matches[4] + " Unsupported`n"
        $output = [Console]::OpenStandardOutput()
        try { $bytes = [Text.Encoding]::ASCII.GetBytes($line); $output.Write($bytes, 0, $bytes.Length); $output.Flush() } finally { $output.Dispose() }
    }
    return 2
}
function Invoke-Ktu2($stream, $header, $body, $bash, $native, $powerShellAccountHome, $envProfile, $q, $frequency) {
    if ($PSVersionTable.PSVersion.Major -ne 5 -or $PSVersionTable.PSVersion.Minor -ne 1 -or $ExecutionContext.SessionState.LanguageMode -cne "FullLanguage") { return (Reject-Ktu2Unsupported $stream $header $body) }
    try { Add-Type -TypeDefinition $script:Ktu2Bridge -ErrorAction Stop | Out-Null } catch { return (Reject-Ktu2Unsupported $stream $header $body) }
    return [Ktu2]::Run($stream, $header, $body, $bash, $native, $powerShellAccountHome, $envProfile, $q, $frequency)
}
