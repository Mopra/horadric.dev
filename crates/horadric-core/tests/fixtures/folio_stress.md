# Folio stress: what a reply should not be able to break

File references to click: crates/horadric-core/src/markdown.rs:812, src/main.rs:3:14, `crates/horadric-ui/src/folio/rich.rs:457`, a Windows one C:\Users\me\repo\build.rs:7 and main.rs:42 on its own.

## A table twenty columns wide

| C1 | C2 | C3 | C4 | C5 | C6 | C7 | C8 | C9 | C10 | C11 | C12 | C13 | C14 | C15 | C16 | C17 | C18 | C19 | C20 |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| r1c1 | r1c2 | r1c3 | r1c4 | r1c5 | r1c6 | r1c7 | r1c8 | r1c9 | r1c10 | r1c11 | r1c12 | r1c13 | r1c14 | r1c15 | r1c16 | r1c17 | r1c18 | r1c19 | r1c20 |
| r2c1 | r2c2 | r2c3 | r2c4 | r2c5 | r2c6 | r2c7 | r2c8 | r2c9 | r2c10 | r2c11 | r2c12 | r2c13 | r2c14 | r2c15 | r2c16 | r2c17 | r2c18 | r2c19 | r2c20 |
| r3c1 | r3c2 | r3c3 | r3c4 | r3c5 | r3c6 | r3c7 | r3c8 | r3c9 | r3c10 | r3c11 | r3c12 | r3c13 | r3c14 | r3c15 | r3c16 | r3c17 | r3c18 | r3c19 | r3c20 |
| r4c1 | r4c2 | r4c3 | r4c4 | r4c5 | r4c6 | r4c7 | r4c8 | r4c9 | r4c10 | r4c11 | r4c12 | r4c13 | r4c14 | r4c15 | r4c16 | r4c17 | r4c18 | r4c19 | r4c20 |

## A table with very long cells

| Key | Value | Note |
|:---|:---|---:|
| short | word1 word2 word3 word4 word5 word6 word7 word8 word9 word10 word11 word12 word13 word14 word15 word16 word17 word18 word19 word20 word21 word22 word23 word24 word25 word26 word27 word28 word29 word30 word31 word32 word33 word34 word35 word36 word37 word38 word39 word40 | `crates/horadric-core/src/markdown.rs:1200` |
| unbroken | xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx | ok |
| mixed | **bold** and *italic* and `a | b` in code and https://horadric.dev/a/very/long/address/that/goes/on/and/on | 3 |

## Deep nesting

> A quote holding a list:
>
> - an item holding code:
>
>   ```rust
>   fn nested() -> u8 { 3 }
>   ```
>
>   - a nested item
>     > a quote in it
>     >
>     > 1. and an ordered list
>     >
>     >    ```sh
>     >    cargo test -p horadric-core markdown
>     >    ```

## Scripts and wide characters

中文段落：这是一个用于测试换行的很长的中文句子，没有空格，所以只能在字符之间断开。日本語の文も混ぜます。한국어 문장도 있습니다.

עברית: זהו משפט בעברית עם **מילה מודגשת** ו־`code` באמצע. العربية: هذه جملة باللغة العربية مع رابط https://horadric.dev في الوسط.

Wide emoji: 👩‍👩‍👧‍👦 🧑🏽‍💻 🏳️‍🌈 🇩🇰 ❤️‍🔥 and **bold 🚀 emoji** and `code 😀`.

## Many footnotes

Footnotes everywhere: claim1[^n1] claim2[^n2] claim3[^n3] claim4[^n4] claim5[^n5] claim6[^n6] claim7[^n7] claim8[^n8] claim9[^n9] claim10[^n10] claim11[^n11] claim12[^n12] claim13[^n13] claim14[^n14] claim15[^n15] claim16[^n16] claim17[^n17] claim18[^n18] claim19[^n19] claim20[^n20] claim21[^n21] claim22[^n22] claim23[^n23] claim24[^n24] claim25[^n25] claim26[^n26] claim27[^n27] claim28[^n28] claim29[^n29] claim30[^n30] claim31[^n31] claim32[^n32] claim33[^n33] claim34[^n34] claim35[^n35] claim36[^n36] claim37[^n37] claim38[^n38] claim39[^n39] claim40[^n40].

[^n1]: Note 1, pointing at src/notes/n1.rs:1.
[^n2]: Note 2, pointing at src/notes/n2.rs:2.
[^n3]: Note 3, pointing at src/notes/n3.rs:3.
[^n4]: Note 4, pointing at src/notes/n4.rs:4.
[^n5]: Note 5, pointing at src/notes/n5.rs:5.
[^n6]: Note 6, pointing at src/notes/n6.rs:6.
[^n7]: Note 7, pointing at src/notes/n7.rs:7.
[^n8]: Note 8, pointing at src/notes/n8.rs:8.
[^n9]: Note 9, pointing at src/notes/n9.rs:9.
[^n10]: Note 10, pointing at src/notes/n10.rs:10.
[^n11]: Note 11, pointing at src/notes/n11.rs:11.
[^n12]: Note 12, pointing at src/notes/n12.rs:12.
[^n13]: Note 13, pointing at src/notes/n13.rs:13.
[^n14]: Note 14, pointing at src/notes/n14.rs:14.
[^n15]: Note 15, pointing at src/notes/n15.rs:15.
[^n16]: Note 16, pointing at src/notes/n16.rs:16.
[^n17]: Note 17, pointing at src/notes/n17.rs:17.
[^n18]: Note 18, pointing at src/notes/n18.rs:18.
[^n19]: Note 19, pointing at src/notes/n19.rs:19.
[^n20]: Note 20, pointing at src/notes/n20.rs:20.
[^n21]: Note 21, pointing at src/notes/n21.rs:21.
[^n22]: Note 22, pointing at src/notes/n22.rs:22.
[^n23]: Note 23, pointing at src/notes/n23.rs:23.
[^n24]: Note 24, pointing at src/notes/n24.rs:24.
[^n25]: Note 25, pointing at src/notes/n25.rs:25.
[^n26]: Note 26, pointing at src/notes/n26.rs:26.
[^n27]: Note 27, pointing at src/notes/n27.rs:27.
[^n28]: Note 28, pointing at src/notes/n28.rs:28.
[^n29]: Note 29, pointing at src/notes/n29.rs:29.
[^n30]: Note 30, pointing at src/notes/n30.rs:30.
[^n31]: Note 31, pointing at src/notes/n31.rs:31.
[^n32]: Note 32, pointing at src/notes/n32.rs:32.
[^n33]: Note 33, pointing at src/notes/n33.rs:33.
[^n34]: Note 34, pointing at src/notes/n34.rs:34.
[^n35]: Note 35, pointing at src/notes/n35.rs:35.
[^n36]: Note 36, pointing at src/notes/n36.rs:36.
[^n37]: Note 37, pointing at src/notes/n37.rs:37.
[^n38]: Note 38, pointing at src/notes/n38.rs:38.
[^n39]: Note 39, pointing at src/notes/n39.rs:39.
[^n40]: Note 40, pointing at src/notes/n40.rs:40.

## A code block five hundred lines long

```rust
    let line_1 = compute(1, "**not bold** [not](a link) 1");
    let line_2 = compute(2, "**not bold** [not](a link) 2");
    let line_3 = compute(3, "**not bold** [not](a link) 3");
    let line_4 = compute(4, "**not bold** [not](a link) 4");
    let line_5 = compute(5, "**not bold** [not](a link) 5");
    let line_6 = compute(6, "**not bold** [not](a link) 6");
    let line_7 = compute(7, "**not bold** [not](a link) 7");
    let line_8 = compute(8, "**not bold** [not](a link) 8");
    let line_9 = compute(9, "**not bold** [not](a link) 9");
    let line_10 = compute(10, "**not bold** [not](a link) 10");
    let line_11 = compute(11, "**not bold** [not](a link) 11");
    let line_12 = compute(12, "**not bold** [not](a link) 12");
    let line_13 = compute(13, "**not bold** [not](a link) 13");
    let line_14 = compute(14, "**not bold** [not](a link) 14");
    let line_15 = compute(15, "**not bold** [not](a link) 15");
    let line_16 = compute(16, "**not bold** [not](a link) 16");
    let line_17 = compute(17, "**not bold** [not](a link) 17");
    let line_18 = compute(18, "**not bold** [not](a link) 18");
    let line_19 = compute(19, "**not bold** [not](a link) 19");
    let line_20 = compute(20, "**not bold** [not](a link) 20");
    let line_21 = compute(21, "**not bold** [not](a link) 21");
    let line_22 = compute(22, "**not bold** [not](a link) 22");
    let line_23 = compute(23, "**not bold** [not](a link) 23");
    let line_24 = compute(24, "**not bold** [not](a link) 24");
    let line_25 = compute(25, "**not bold** [not](a link) 25");
    let line_26 = compute(26, "**not bold** [not](a link) 26");
    let line_27 = compute(27, "**not bold** [not](a link) 27");
    let line_28 = compute(28, "**not bold** [not](a link) 28");
    let line_29 = compute(29, "**not bold** [not](a link) 29");
    let line_30 = compute(30, "**not bold** [not](a link) 30");
    let line_31 = compute(31, "**not bold** [not](a link) 31");
    let line_32 = compute(32, "**not bold** [not](a link) 32");
    let line_33 = compute(33, "**not bold** [not](a link) 33");
    let line_34 = compute(34, "**not bold** [not](a link) 34");
    let line_35 = compute(35, "**not bold** [not](a link) 35");
    let line_36 = compute(36, "**not bold** [not](a link) 36");
    let line_37 = compute(37, "**not bold** [not](a link) 37");
    let line_38 = compute(38, "**not bold** [not](a link) 38");
    let line_39 = compute(39, "**not bold** [not](a link) 39");
    let line_40 = compute(40, "**not bold** [not](a link) 40");
    let line_41 = compute(41, "**not bold** [not](a link) 41");
    let line_42 = compute(42, "**not bold** [not](a link) 42");
    let line_43 = compute(43, "**not bold** [not](a link) 43");
    let line_44 = compute(44, "**not bold** [not](a link) 44");
    let line_45 = compute(45, "**not bold** [not](a link) 45");
    let line_46 = compute(46, "**not bold** [not](a link) 46");
    let line_47 = compute(47, "**not bold** [not](a link) 47");
    let line_48 = compute(48, "**not bold** [not](a link) 48");
    let line_49 = compute(49, "**not bold** [not](a link) 49");
    let line_50 = compute(50, "**not bold** [not](a link) 50");
    let line_51 = compute(51, "**not bold** [not](a link) 51");
    let line_52 = compute(52, "**not bold** [not](a link) 52");
    let line_53 = compute(53, "**not bold** [not](a link) 53");
    let line_54 = compute(54, "**not bold** [not](a link) 54");
    let line_55 = compute(55, "**not bold** [not](a link) 55");
    let line_56 = compute(56, "**not bold** [not](a link) 56");
    let line_57 = compute(57, "**not bold** [not](a link) 57");
    let line_58 = compute(58, "**not bold** [not](a link) 58");
    let line_59 = compute(59, "**not bold** [not](a link) 59");
    let line_60 = compute(60, "**not bold** [not](a link) 60");
    let line_61 = compute(61, "**not bold** [not](a link) 61");
    let line_62 = compute(62, "**not bold** [not](a link) 62");
    let line_63 = compute(63, "**not bold** [not](a link) 63");
    let line_64 = compute(64, "**not bold** [not](a link) 64");
    let line_65 = compute(65, "**not bold** [not](a link) 65");
    let line_66 = compute(66, "**not bold** [not](a link) 66");
    let line_67 = compute(67, "**not bold** [not](a link) 67");
    let line_68 = compute(68, "**not bold** [not](a link) 68");
    let line_69 = compute(69, "**not bold** [not](a link) 69");
    let line_70 = compute(70, "**not bold** [not](a link) 70");
    let line_71 = compute(71, "**not bold** [not](a link) 71");
    let line_72 = compute(72, "**not bold** [not](a link) 72");
    let line_73 = compute(73, "**not bold** [not](a link) 73");
    let line_74 = compute(74, "**not bold** [not](a link) 74");
    let line_75 = compute(75, "**not bold** [not](a link) 75");
    let line_76 = compute(76, "**not bold** [not](a link) 76");
    let line_77 = compute(77, "**not bold** [not](a link) 77");
    let line_78 = compute(78, "**not bold** [not](a link) 78");
    let line_79 = compute(79, "**not bold** [not](a link) 79");
    let line_80 = compute(80, "**not bold** [not](a link) 80");
    let line_81 = compute(81, "**not bold** [not](a link) 81");
    let line_82 = compute(82, "**not bold** [not](a link) 82");
    let line_83 = compute(83, "**not bold** [not](a link) 83");
    let line_84 = compute(84, "**not bold** [not](a link) 84");
    let line_85 = compute(85, "**not bold** [not](a link) 85");
    let line_86 = compute(86, "**not bold** [not](a link) 86");
    let line_87 = compute(87, "**not bold** [not](a link) 87");
    let line_88 = compute(88, "**not bold** [not](a link) 88");
    let line_89 = compute(89, "**not bold** [not](a link) 89");
    let line_90 = compute(90, "**not bold** [not](a link) 90");
    let line_91 = compute(91, "**not bold** [not](a link) 91");
    let line_92 = compute(92, "**not bold** [not](a link) 92");
    let line_93 = compute(93, "**not bold** [not](a link) 93");
    let line_94 = compute(94, "**not bold** [not](a link) 94");
    let line_95 = compute(95, "**not bold** [not](a link) 95");
    let line_96 = compute(96, "**not bold** [not](a link) 96");
    let line_97 = compute(97, "**not bold** [not](a link) 97");
    let line_98 = compute(98, "**not bold** [not](a link) 98");
    let line_99 = compute(99, "**not bold** [not](a link) 99");
    let line_100 = compute(100, "**not bold** [not](a link) 100");
    let line_101 = compute(101, "**not bold** [not](a link) 101");
    let line_102 = compute(102, "**not bold** [not](a link) 102");
    let line_103 = compute(103, "**not bold** [not](a link) 103");
    let line_104 = compute(104, "**not bold** [not](a link) 104");
    let line_105 = compute(105, "**not bold** [not](a link) 105");
    let line_106 = compute(106, "**not bold** [not](a link) 106");
    let line_107 = compute(107, "**not bold** [not](a link) 107");
    let line_108 = compute(108, "**not bold** [not](a link) 108");
    let line_109 = compute(109, "**not bold** [not](a link) 109");
    let line_110 = compute(110, "**not bold** [not](a link) 110");
    let line_111 = compute(111, "**not bold** [not](a link) 111");
    let line_112 = compute(112, "**not bold** [not](a link) 112");
    let line_113 = compute(113, "**not bold** [not](a link) 113");
    let line_114 = compute(114, "**not bold** [not](a link) 114");
    let line_115 = compute(115, "**not bold** [not](a link) 115");
    let line_116 = compute(116, "**not bold** [not](a link) 116");
    let line_117 = compute(117, "**not bold** [not](a link) 117");
    let line_118 = compute(118, "**not bold** [not](a link) 118");
    let line_119 = compute(119, "**not bold** [not](a link) 119");
    let line_120 = compute(120, "**not bold** [not](a link) 120");
    let line_121 = compute(121, "**not bold** [not](a link) 121");
    let line_122 = compute(122, "**not bold** [not](a link) 122");
    let line_123 = compute(123, "**not bold** [not](a link) 123");
    let line_124 = compute(124, "**not bold** [not](a link) 124");
    let line_125 = compute(125, "**not bold** [not](a link) 125");
    let line_126 = compute(126, "**not bold** [not](a link) 126");
    let line_127 = compute(127, "**not bold** [not](a link) 127");
    let line_128 = compute(128, "**not bold** [not](a link) 128");
    let line_129 = compute(129, "**not bold** [not](a link) 129");
    let line_130 = compute(130, "**not bold** [not](a link) 130");
    let line_131 = compute(131, "**not bold** [not](a link) 131");
    let line_132 = compute(132, "**not bold** [not](a link) 132");
    let line_133 = compute(133, "**not bold** [not](a link) 133");
    let line_134 = compute(134, "**not bold** [not](a link) 134");
    let line_135 = compute(135, "**not bold** [not](a link) 135");
    let line_136 = compute(136, "**not bold** [not](a link) 136");
    let line_137 = compute(137, "**not bold** [not](a link) 137");
    let line_138 = compute(138, "**not bold** [not](a link) 138");
    let line_139 = compute(139, "**not bold** [not](a link) 139");
    let line_140 = compute(140, "**not bold** [not](a link) 140");
    let line_141 = compute(141, "**not bold** [not](a link) 141");
    let line_142 = compute(142, "**not bold** [not](a link) 142");
    let line_143 = compute(143, "**not bold** [not](a link) 143");
    let line_144 = compute(144, "**not bold** [not](a link) 144");
    let line_145 = compute(145, "**not bold** [not](a link) 145");
    let line_146 = compute(146, "**not bold** [not](a link) 146");
    let line_147 = compute(147, "**not bold** [not](a link) 147");
    let line_148 = compute(148, "**not bold** [not](a link) 148");
    let line_149 = compute(149, "**not bold** [not](a link) 149");
    let line_150 = compute(150, "**not bold** [not](a link) 150");
    let line_151 = compute(151, "**not bold** [not](a link) 151");
    let line_152 = compute(152, "**not bold** [not](a link) 152");
    let line_153 = compute(153, "**not bold** [not](a link) 153");
    let line_154 = compute(154, "**not bold** [not](a link) 154");
    let line_155 = compute(155, "**not bold** [not](a link) 155");
    let line_156 = compute(156, "**not bold** [not](a link) 156");
    let line_157 = compute(157, "**not bold** [not](a link) 157");
    let line_158 = compute(158, "**not bold** [not](a link) 158");
    let line_159 = compute(159, "**not bold** [not](a link) 159");
    let line_160 = compute(160, "**not bold** [not](a link) 160");
    let line_161 = compute(161, "**not bold** [not](a link) 161");
    let line_162 = compute(162, "**not bold** [not](a link) 162");
    let line_163 = compute(163, "**not bold** [not](a link) 163");
    let line_164 = compute(164, "**not bold** [not](a link) 164");
    let line_165 = compute(165, "**not bold** [not](a link) 165");
    let line_166 = compute(166, "**not bold** [not](a link) 166");
    let line_167 = compute(167, "**not bold** [not](a link) 167");
    let line_168 = compute(168, "**not bold** [not](a link) 168");
    let line_169 = compute(169, "**not bold** [not](a link) 169");
    let line_170 = compute(170, "**not bold** [not](a link) 170");
    let line_171 = compute(171, "**not bold** [not](a link) 171");
    let line_172 = compute(172, "**not bold** [not](a link) 172");
    let line_173 = compute(173, "**not bold** [not](a link) 173");
    let line_174 = compute(174, "**not bold** [not](a link) 174");
    let line_175 = compute(175, "**not bold** [not](a link) 175");
    let line_176 = compute(176, "**not bold** [not](a link) 176");
    let line_177 = compute(177, "**not bold** [not](a link) 177");
    let line_178 = compute(178, "**not bold** [not](a link) 178");
    let line_179 = compute(179, "**not bold** [not](a link) 179");
    let line_180 = compute(180, "**not bold** [not](a link) 180");
    let line_181 = compute(181, "**not bold** [not](a link) 181");
    let line_182 = compute(182, "**not bold** [not](a link) 182");
    let line_183 = compute(183, "**not bold** [not](a link) 183");
    let line_184 = compute(184, "**not bold** [not](a link) 184");
    let line_185 = compute(185, "**not bold** [not](a link) 185");
    let line_186 = compute(186, "**not bold** [not](a link) 186");
    let line_187 = compute(187, "**not bold** [not](a link) 187");
    let line_188 = compute(188, "**not bold** [not](a link) 188");
    let line_189 = compute(189, "**not bold** [not](a link) 189");
    let line_190 = compute(190, "**not bold** [not](a link) 190");
    let line_191 = compute(191, "**not bold** [not](a link) 191");
    let line_192 = compute(192, "**not bold** [not](a link) 192");
    let line_193 = compute(193, "**not bold** [not](a link) 193");
    let line_194 = compute(194, "**not bold** [not](a link) 194");
    let line_195 = compute(195, "**not bold** [not](a link) 195");
    let line_196 = compute(196, "**not bold** [not](a link) 196");
    let line_197 = compute(197, "**not bold** [not](a link) 197");
    let line_198 = compute(198, "**not bold** [not](a link) 198");
    let line_199 = compute(199, "**not bold** [not](a link) 199");
    let line_200 = compute(200, "**not bold** [not](a link) 200");
    let line_201 = compute(201, "**not bold** [not](a link) 201");
    let line_202 = compute(202, "**not bold** [not](a link) 202");
    let line_203 = compute(203, "**not bold** [not](a link) 203");
    let line_204 = compute(204, "**not bold** [not](a link) 204");
    let line_205 = compute(205, "**not bold** [not](a link) 205");
    let line_206 = compute(206, "**not bold** [not](a link) 206");
    let line_207 = compute(207, "**not bold** [not](a link) 207");
    let line_208 = compute(208, "**not bold** [not](a link) 208");
    let line_209 = compute(209, "**not bold** [not](a link) 209");
    let line_210 = compute(210, "**not bold** [not](a link) 210");
    let line_211 = compute(211, "**not bold** [not](a link) 211");
    let line_212 = compute(212, "**not bold** [not](a link) 212");
    let line_213 = compute(213, "**not bold** [not](a link) 213");
    let line_214 = compute(214, "**not bold** [not](a link) 214");
    let line_215 = compute(215, "**not bold** [not](a link) 215");
    let line_216 = compute(216, "**not bold** [not](a link) 216");
    let line_217 = compute(217, "**not bold** [not](a link) 217");
    let line_218 = compute(218, "**not bold** [not](a link) 218");
    let line_219 = compute(219, "**not bold** [not](a link) 219");
    let line_220 = compute(220, "**not bold** [not](a link) 220");
    let line_221 = compute(221, "**not bold** [not](a link) 221");
    let line_222 = compute(222, "**not bold** [not](a link) 222");
    let line_223 = compute(223, "**not bold** [not](a link) 223");
    let line_224 = compute(224, "**not bold** [not](a link) 224");
    let line_225 = compute(225, "**not bold** [not](a link) 225");
    let line_226 = compute(226, "**not bold** [not](a link) 226");
    let line_227 = compute(227, "**not bold** [not](a link) 227");
    let line_228 = compute(228, "**not bold** [not](a link) 228");
    let line_229 = compute(229, "**not bold** [not](a link) 229");
    let line_230 = compute(230, "**not bold** [not](a link) 230");
    let line_231 = compute(231, "**not bold** [not](a link) 231");
    let line_232 = compute(232, "**not bold** [not](a link) 232");
    let line_233 = compute(233, "**not bold** [not](a link) 233");
    let line_234 = compute(234, "**not bold** [not](a link) 234");
    let line_235 = compute(235, "**not bold** [not](a link) 235");
    let line_236 = compute(236, "**not bold** [not](a link) 236");
    let line_237 = compute(237, "**not bold** [not](a link) 237");
    let line_238 = compute(238, "**not bold** [not](a link) 238");
    let line_239 = compute(239, "**not bold** [not](a link) 239");
    let line_240 = compute(240, "**not bold** [not](a link) 240");
    let line_241 = compute(241, "**not bold** [not](a link) 241");
    let line_242 = compute(242, "**not bold** [not](a link) 242");
    let line_243 = compute(243, "**not bold** [not](a link) 243");
    let line_244 = compute(244, "**not bold** [not](a link) 244");
    let line_245 = compute(245, "**not bold** [not](a link) 245");
    let line_246 = compute(246, "**not bold** [not](a link) 246");
    let line_247 = compute(247, "**not bold** [not](a link) 247");
    let line_248 = compute(248, "**not bold** [not](a link) 248");
    let line_249 = compute(249, "**not bold** [not](a link) 249");
    let line_250 = compute(250, "**not bold** [not](a link) 250");
    let line_251 = compute(251, "**not bold** [not](a link) 251");
    let line_252 = compute(252, "**not bold** [not](a link) 252");
    let line_253 = compute(253, "**not bold** [not](a link) 253");
    let line_254 = compute(254, "**not bold** [not](a link) 254");
    let line_255 = compute(255, "**not bold** [not](a link) 255");
    let line_256 = compute(256, "**not bold** [not](a link) 256");
    let line_257 = compute(257, "**not bold** [not](a link) 257");
    let line_258 = compute(258, "**not bold** [not](a link) 258");
    let line_259 = compute(259, "**not bold** [not](a link) 259");
    let line_260 = compute(260, "**not bold** [not](a link) 260");
    let line_261 = compute(261, "**not bold** [not](a link) 261");
    let line_262 = compute(262, "**not bold** [not](a link) 262");
    let line_263 = compute(263, "**not bold** [not](a link) 263");
    let line_264 = compute(264, "**not bold** [not](a link) 264");
    let line_265 = compute(265, "**not bold** [not](a link) 265");
    let line_266 = compute(266, "**not bold** [not](a link) 266");
    let line_267 = compute(267, "**not bold** [not](a link) 267");
    let line_268 = compute(268, "**not bold** [not](a link) 268");
    let line_269 = compute(269, "**not bold** [not](a link) 269");
    let line_270 = compute(270, "**not bold** [not](a link) 270");
    let line_271 = compute(271, "**not bold** [not](a link) 271");
    let line_272 = compute(272, "**not bold** [not](a link) 272");
    let line_273 = compute(273, "**not bold** [not](a link) 273");
    let line_274 = compute(274, "**not bold** [not](a link) 274");
    let line_275 = compute(275, "**not bold** [not](a link) 275");
    let line_276 = compute(276, "**not bold** [not](a link) 276");
    let line_277 = compute(277, "**not bold** [not](a link) 277");
    let line_278 = compute(278, "**not bold** [not](a link) 278");
    let line_279 = compute(279, "**not bold** [not](a link) 279");
    let line_280 = compute(280, "**not bold** [not](a link) 280");
    let line_281 = compute(281, "**not bold** [not](a link) 281");
    let line_282 = compute(282, "**not bold** [not](a link) 282");
    let line_283 = compute(283, "**not bold** [not](a link) 283");
    let line_284 = compute(284, "**not bold** [not](a link) 284");
    let line_285 = compute(285, "**not bold** [not](a link) 285");
    let line_286 = compute(286, "**not bold** [not](a link) 286");
    let line_287 = compute(287, "**not bold** [not](a link) 287");
    let line_288 = compute(288, "**not bold** [not](a link) 288");
    let line_289 = compute(289, "**not bold** [not](a link) 289");
    let line_290 = compute(290, "**not bold** [not](a link) 290");
    let line_291 = compute(291, "**not bold** [not](a link) 291");
    let line_292 = compute(292, "**not bold** [not](a link) 292");
    let line_293 = compute(293, "**not bold** [not](a link) 293");
    let line_294 = compute(294, "**not bold** [not](a link) 294");
    let line_295 = compute(295, "**not bold** [not](a link) 295");
    let line_296 = compute(296, "**not bold** [not](a link) 296");
    let line_297 = compute(297, "**not bold** [not](a link) 297");
    let line_298 = compute(298, "**not bold** [not](a link) 298");
    let line_299 = compute(299, "**not bold** [not](a link) 299");
    let line_300 = compute(300, "**not bold** [not](a link) 300");
    let line_301 = compute(301, "**not bold** [not](a link) 301");
    let line_302 = compute(302, "**not bold** [not](a link) 302");
    let line_303 = compute(303, "**not bold** [not](a link) 303");
    let line_304 = compute(304, "**not bold** [not](a link) 304");
    let line_305 = compute(305, "**not bold** [not](a link) 305");
    let line_306 = compute(306, "**not bold** [not](a link) 306");
    let line_307 = compute(307, "**not bold** [not](a link) 307");
    let line_308 = compute(308, "**not bold** [not](a link) 308");
    let line_309 = compute(309, "**not bold** [not](a link) 309");
    let line_310 = compute(310, "**not bold** [not](a link) 310");
    let line_311 = compute(311, "**not bold** [not](a link) 311");
    let line_312 = compute(312, "**not bold** [not](a link) 312");
    let line_313 = compute(313, "**not bold** [not](a link) 313");
    let line_314 = compute(314, "**not bold** [not](a link) 314");
    let line_315 = compute(315, "**not bold** [not](a link) 315");
    let line_316 = compute(316, "**not bold** [not](a link) 316");
    let line_317 = compute(317, "**not bold** [not](a link) 317");
    let line_318 = compute(318, "**not bold** [not](a link) 318");
    let line_319 = compute(319, "**not bold** [not](a link) 319");
    let line_320 = compute(320, "**not bold** [not](a link) 320");
    let line_321 = compute(321, "**not bold** [not](a link) 321");
    let line_322 = compute(322, "**not bold** [not](a link) 322");
    let line_323 = compute(323, "**not bold** [not](a link) 323");
    let line_324 = compute(324, "**not bold** [not](a link) 324");
    let line_325 = compute(325, "**not bold** [not](a link) 325");
    let line_326 = compute(326, "**not bold** [not](a link) 326");
    let line_327 = compute(327, "**not bold** [not](a link) 327");
    let line_328 = compute(328, "**not bold** [not](a link) 328");
    let line_329 = compute(329, "**not bold** [not](a link) 329");
    let line_330 = compute(330, "**not bold** [not](a link) 330");
    let line_331 = compute(331, "**not bold** [not](a link) 331");
    let line_332 = compute(332, "**not bold** [not](a link) 332");
    let line_333 = compute(333, "**not bold** [not](a link) 333");
    let line_334 = compute(334, "**not bold** [not](a link) 334");
    let line_335 = compute(335, "**not bold** [not](a link) 335");
    let line_336 = compute(336, "**not bold** [not](a link) 336");
    let line_337 = compute(337, "**not bold** [not](a link) 337");
    let line_338 = compute(338, "**not bold** [not](a link) 338");
    let line_339 = compute(339, "**not bold** [not](a link) 339");
    let line_340 = compute(340, "**not bold** [not](a link) 340");
    let line_341 = compute(341, "**not bold** [not](a link) 341");
    let line_342 = compute(342, "**not bold** [not](a link) 342");
    let line_343 = compute(343, "**not bold** [not](a link) 343");
    let line_344 = compute(344, "**not bold** [not](a link) 344");
    let line_345 = compute(345, "**not bold** [not](a link) 345");
    let line_346 = compute(346, "**not bold** [not](a link) 346");
    let line_347 = compute(347, "**not bold** [not](a link) 347");
    let line_348 = compute(348, "**not bold** [not](a link) 348");
    let line_349 = compute(349, "**not bold** [not](a link) 349");
    let line_350 = compute(350, "**not bold** [not](a link) 350");
    let line_351 = compute(351, "**not bold** [not](a link) 351");
    let line_352 = compute(352, "**not bold** [not](a link) 352");
    let line_353 = compute(353, "**not bold** [not](a link) 353");
    let line_354 = compute(354, "**not bold** [not](a link) 354");
    let line_355 = compute(355, "**not bold** [not](a link) 355");
    let line_356 = compute(356, "**not bold** [not](a link) 356");
    let line_357 = compute(357, "**not bold** [not](a link) 357");
    let line_358 = compute(358, "**not bold** [not](a link) 358");
    let line_359 = compute(359, "**not bold** [not](a link) 359");
    let line_360 = compute(360, "**not bold** [not](a link) 360");
    let line_361 = compute(361, "**not bold** [not](a link) 361");
    let line_362 = compute(362, "**not bold** [not](a link) 362");
    let line_363 = compute(363, "**not bold** [not](a link) 363");
    let line_364 = compute(364, "**not bold** [not](a link) 364");
    let line_365 = compute(365, "**not bold** [not](a link) 365");
    let line_366 = compute(366, "**not bold** [not](a link) 366");
    let line_367 = compute(367, "**not bold** [not](a link) 367");
    let line_368 = compute(368, "**not bold** [not](a link) 368");
    let line_369 = compute(369, "**not bold** [not](a link) 369");
    let line_370 = compute(370, "**not bold** [not](a link) 370");
    let line_371 = compute(371, "**not bold** [not](a link) 371");
    let line_372 = compute(372, "**not bold** [not](a link) 372");
    let line_373 = compute(373, "**not bold** [not](a link) 373");
    let line_374 = compute(374, "**not bold** [not](a link) 374");
    let line_375 = compute(375, "**not bold** [not](a link) 375");
    let line_376 = compute(376, "**not bold** [not](a link) 376");
    let line_377 = compute(377, "**not bold** [not](a link) 377");
    let line_378 = compute(378, "**not bold** [not](a link) 378");
    let line_379 = compute(379, "**not bold** [not](a link) 379");
    let line_380 = compute(380, "**not bold** [not](a link) 380");
    let line_381 = compute(381, "**not bold** [not](a link) 381");
    let line_382 = compute(382, "**not bold** [not](a link) 382");
    let line_383 = compute(383, "**not bold** [not](a link) 383");
    let line_384 = compute(384, "**not bold** [not](a link) 384");
    let line_385 = compute(385, "**not bold** [not](a link) 385");
    let line_386 = compute(386, "**not bold** [not](a link) 386");
    let line_387 = compute(387, "**not bold** [not](a link) 387");
    let line_388 = compute(388, "**not bold** [not](a link) 388");
    let line_389 = compute(389, "**not bold** [not](a link) 389");
    let line_390 = compute(390, "**not bold** [not](a link) 390");
    let line_391 = compute(391, "**not bold** [not](a link) 391");
    let line_392 = compute(392, "**not bold** [not](a link) 392");
    let line_393 = compute(393, "**not bold** [not](a link) 393");
    let line_394 = compute(394, "**not bold** [not](a link) 394");
    let line_395 = compute(395, "**not bold** [not](a link) 395");
    let line_396 = compute(396, "**not bold** [not](a link) 396");
    let line_397 = compute(397, "**not bold** [not](a link) 397");
    let line_398 = compute(398, "**not bold** [not](a link) 398");
    let line_399 = compute(399, "**not bold** [not](a link) 399");
    let line_400 = compute(400, "**not bold** [not](a link) 400");
    let line_401 = compute(401, "**not bold** [not](a link) 401");
    let line_402 = compute(402, "**not bold** [not](a link) 402");
    let line_403 = compute(403, "**not bold** [not](a link) 403");
    let line_404 = compute(404, "**not bold** [not](a link) 404");
    let line_405 = compute(405, "**not bold** [not](a link) 405");
    let line_406 = compute(406, "**not bold** [not](a link) 406");
    let line_407 = compute(407, "**not bold** [not](a link) 407");
    let line_408 = compute(408, "**not bold** [not](a link) 408");
    let line_409 = compute(409, "**not bold** [not](a link) 409");
    let line_410 = compute(410, "**not bold** [not](a link) 410");
    let line_411 = compute(411, "**not bold** [not](a link) 411");
    let line_412 = compute(412, "**not bold** [not](a link) 412");
    let line_413 = compute(413, "**not bold** [not](a link) 413");
    let line_414 = compute(414, "**not bold** [not](a link) 414");
    let line_415 = compute(415, "**not bold** [not](a link) 415");
    let line_416 = compute(416, "**not bold** [not](a link) 416");
    let line_417 = compute(417, "**not bold** [not](a link) 417");
    let line_418 = compute(418, "**not bold** [not](a link) 418");
    let line_419 = compute(419, "**not bold** [not](a link) 419");
    let line_420 = compute(420, "**not bold** [not](a link) 420");
    let line_421 = compute(421, "**not bold** [not](a link) 421");
    let line_422 = compute(422, "**not bold** [not](a link) 422");
    let line_423 = compute(423, "**not bold** [not](a link) 423");
    let line_424 = compute(424, "**not bold** [not](a link) 424");
    let line_425 = compute(425, "**not bold** [not](a link) 425");
    let line_426 = compute(426, "**not bold** [not](a link) 426");
    let line_427 = compute(427, "**not bold** [not](a link) 427");
    let line_428 = compute(428, "**not bold** [not](a link) 428");
    let line_429 = compute(429, "**not bold** [not](a link) 429");
    let line_430 = compute(430, "**not bold** [not](a link) 430");
    let line_431 = compute(431, "**not bold** [not](a link) 431");
    let line_432 = compute(432, "**not bold** [not](a link) 432");
    let line_433 = compute(433, "**not bold** [not](a link) 433");
    let line_434 = compute(434, "**not bold** [not](a link) 434");
    let line_435 = compute(435, "**not bold** [not](a link) 435");
    let line_436 = compute(436, "**not bold** [not](a link) 436");
    let line_437 = compute(437, "**not bold** [not](a link) 437");
    let line_438 = compute(438, "**not bold** [not](a link) 438");
    let line_439 = compute(439, "**not bold** [not](a link) 439");
    let line_440 = compute(440, "**not bold** [not](a link) 440");
    let line_441 = compute(441, "**not bold** [not](a link) 441");
    let line_442 = compute(442, "**not bold** [not](a link) 442");
    let line_443 = compute(443, "**not bold** [not](a link) 443");
    let line_444 = compute(444, "**not bold** [not](a link) 444");
    let line_445 = compute(445, "**not bold** [not](a link) 445");
    let line_446 = compute(446, "**not bold** [not](a link) 446");
    let line_447 = compute(447, "**not bold** [not](a link) 447");
    let line_448 = compute(448, "**not bold** [not](a link) 448");
    let line_449 = compute(449, "**not bold** [not](a link) 449");
    let line_450 = compute(450, "**not bold** [not](a link) 450");
    let line_451 = compute(451, "**not bold** [not](a link) 451");
    let line_452 = compute(452, "**not bold** [not](a link) 452");
    let line_453 = compute(453, "**not bold** [not](a link) 453");
    let line_454 = compute(454, "**not bold** [not](a link) 454");
    let line_455 = compute(455, "**not bold** [not](a link) 455");
    let line_456 = compute(456, "**not bold** [not](a link) 456");
    let line_457 = compute(457, "**not bold** [not](a link) 457");
    let line_458 = compute(458, "**not bold** [not](a link) 458");
    let line_459 = compute(459, "**not bold** [not](a link) 459");
    let line_460 = compute(460, "**not bold** [not](a link) 460");
    let line_461 = compute(461, "**not bold** [not](a link) 461");
    let line_462 = compute(462, "**not bold** [not](a link) 462");
    let line_463 = compute(463, "**not bold** [not](a link) 463");
    let line_464 = compute(464, "**not bold** [not](a link) 464");
    let line_465 = compute(465, "**not bold** [not](a link) 465");
    let line_466 = compute(466, "**not bold** [not](a link) 466");
    let line_467 = compute(467, "**not bold** [not](a link) 467");
    let line_468 = compute(468, "**not bold** [not](a link) 468");
    let line_469 = compute(469, "**not bold** [not](a link) 469");
    let line_470 = compute(470, "**not bold** [not](a link) 470");
    let line_471 = compute(471, "**not bold** [not](a link) 471");
    let line_472 = compute(472, "**not bold** [not](a link) 472");
    let line_473 = compute(473, "**not bold** [not](a link) 473");
    let line_474 = compute(474, "**not bold** [not](a link) 474");
    let line_475 = compute(475, "**not bold** [not](a link) 475");
    let line_476 = compute(476, "**not bold** [not](a link) 476");
    let line_477 = compute(477, "**not bold** [not](a link) 477");
    let line_478 = compute(478, "**not bold** [not](a link) 478");
    let line_479 = compute(479, "**not bold** [not](a link) 479");
    let line_480 = compute(480, "**not bold** [not](a link) 480");
    let line_481 = compute(481, "**not bold** [not](a link) 481");
    let line_482 = compute(482, "**not bold** [not](a link) 482");
    let line_483 = compute(483, "**not bold** [not](a link) 483");
    let line_484 = compute(484, "**not bold** [not](a link) 484");
    let line_485 = compute(485, "**not bold** [not](a link) 485");
    let line_486 = compute(486, "**not bold** [not](a link) 486");
    let line_487 = compute(487, "**not bold** [not](a link) 487");
    let line_488 = compute(488, "**not bold** [not](a link) 488");
    let line_489 = compute(489, "**not bold** [not](a link) 489");
    let line_490 = compute(490, "**not bold** [not](a link) 490");
    let line_491 = compute(491, "**not bold** [not](a link) 491");
    let line_492 = compute(492, "**not bold** [not](a link) 492");
    let line_493 = compute(493, "**not bold** [not](a link) 493");
    let line_494 = compute(494, "**not bold** [not](a link) 494");
    let line_495 = compute(495, "**not bold** [not](a link) 495");
    let line_496 = compute(496, "**not bold** [not](a link) 496");
    let line_497 = compute(497, "**not bold** [not](a link) 497");
    let line_498 = compute(498, "**not bold** [not](a link) 498");
    let line_499 = compute(499, "**not bold** [not](a link) 499");
    let line_500 = compute(500, "**not bold** [not](a link) 500");
```

And the reply was cut off in the middle of its last fence:

```python
def unfinished():
    return "still streaming
