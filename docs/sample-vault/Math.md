---
tags: [tour]
aliases: [Formulas]
---

# Math

Formulas are written in LaTeX between dollar signs. Back to [[Welcome]].

## Inline and display

Inline, $e^{i\pi} + 1 = 0$ sits in the line of text. Two dollar signs on
their own lines make a display formula:

$$
\int_0^1 x^2 \, dx = \frac{1}{3}
\qquad
\sum_{k=1}^{n} k = \frac{n(n+1)}{2}
$$

## Macros

Define a command once with `\newcommand` (or `\def`) and use it in every
formula below it in the same note:

$$
\newcommand{\R}{\mathbb{R}}
\newcommand{\norm}[1]{\left\lVert #1 \right\rVert}
\def\half{\tfrac{1}{2}}
f\colon \R^n \to \R, \qquad f(x) = \half \norm{x}^2
$$

The macros keep working later on: $\norm{v} \ge 0$ for every $v \in \R^n$.

## Matrices and cases

$$
A = \begin{pmatrix} 1 & 2 \\ 3 & 4 \end{pmatrix}
\qquad
|x| = \begin{cases} x & \text{if } x \ge 0 \\ -x & \text{otherwise} \end{cases}
$$

## Chemistry

Chemical formulas use `\ce{…}`: $\ce{2H2 + O2 -> 2H2O}$.

## Mistakes

A formula with an error shows a message and its source, instead of
breaking the page: $\frac{1}{$
