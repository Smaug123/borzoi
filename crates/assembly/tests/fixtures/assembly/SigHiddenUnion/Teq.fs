namespace SigHiddenUnion

type Teq<'a, 'b> = private Teq of ('a -> 'b) * ('b -> 'a)
