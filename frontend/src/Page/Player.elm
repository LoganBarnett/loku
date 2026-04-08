module Page.Player exposing (Model, Msg(..), init, update, view, mediaErrorMessage)

import Api exposing (Entry(..))
import Route
import Html exposing (..)
import Html.Attributes exposing (..)
import Html.Events exposing (on)
import Http
import Json.Decode as D


type Model
    = Loading Bool String
    | Loaded PlayerState
    | Failed String


type alias PlayerState =
    { path : String
    , title : String
    , uploadDate : Maybe String
    , durationSecs : Maybe Float
    , compatPath : Maybe String
    , description : Maybe String
    , channel : Maybe String
    , channelUrl : Maybe String
    , webpageUrl : Maybe String
    , viewCount : Maybe Int
    , bufferFraction : Float
    , mediaError : Maybe MediaErrorCode
    , canWebm : Bool
    }


{-| MediaError.code values from the HTML media element spec.
-}
type MediaErrorCode
    = ErrAborted
    | ErrNetwork
    | ErrDecode
    | ErrSrcNotSupported
    | ErrUnknown Int


type Msg
    = GotListing (Result Http.Error Api.DirListing)
    | VideoCanPlay
    | VideoProgress Float
    | MediaError MediaErrorCode


init : Bool -> String -> ( Model, Cmd Msg )
init canWebm path =
    ( Loading canWebm path
    , Api.getBrowse (parentPath path) GotListing
    )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotListing (Ok listing) ->
            let
                path =
                    loadingPath model

                canWebm =
                    loadingCanWebm model
            in
            ( Loaded (findVideo canWebm path listing.entries)
            , Cmd.none
            )

        GotListing (Err _) ->
            ( Loaded
                (fallbackState
                    (loadingCanWebm model)
                    (loadingPath model)
                )
            , Cmd.none
            )

        VideoCanPlay ->
            case model of
                Loaded state ->
                    ( Loaded { state | bufferFraction = 1 }, Cmd.none )

                _ ->
                    ( model, Cmd.none )

        VideoProgress fraction ->
            case model of
                Loaded state ->
                    ( Loaded { state | bufferFraction = fraction }, Cmd.none )

                _ ->
                    ( model, Cmd.none )

        MediaError code ->
            case model of
                Loaded state ->
                    ( Loaded { state | mediaError = Just code }, Cmd.none )

                _ ->
                    ( model, Cmd.none )


{-| Search the directory listing for the video matching the given path
and convert it to a PlayerState.  Falls back to a minimal state when
the video is not found in the listing.
-}
findVideo : Bool -> String -> List Api.Entry -> PlayerState
findVideo canWebm path entries =
    entries
        |> List.filterMap (matchVideo canWebm path)
        |> List.head
        |> Maybe.withDefault (fallbackState canWebm path)


matchVideo : Bool -> String -> Api.Entry -> Maybe PlayerState
matchVideo canWebm path entry =
    case entry of
        Video v ->
            if v.path == path then
                Just
                    { path = v.path
                    , title = Maybe.withDefault v.name v.title
                    , uploadDate = v.uploadDate
                    , durationSecs = v.durationSecs
                    , compatPath = v.compatPath
                    , description = v.description
                    , channel = v.channel
                    , channelUrl = v.channelUrl
                    , webpageUrl = v.webpageUrl
                    , viewCount = v.viewCount
                    , bufferFraction = 0
                    , mediaError = Nothing
                    , canWebm = canWebm
                    }

            else
                Nothing

        _ ->
            Nothing


fallbackState : Bool -> String -> PlayerState
fallbackState canWebm path =
    { path = path
    , title = path
    , uploadDate = Nothing
    , durationSecs = Nothing
    , compatPath = Nothing
    , description = Nothing
    , channel = Nothing
    , channelUrl = Nothing
    , webpageUrl = Nothing
    , viewCount = Nothing
    , bufferFraction = 0
    , mediaError = Nothing
    , canWebm = canWebm
    }



-- VIEW


view : Model -> Html Msg
view model =
    case model of
        Loading _ _ ->
            p [ style "padding" "1rem" ] [ text "Loading…" ]

        Failed err ->
            p [ style "padding" "1rem", style "color" "var(--color-error)" ]
                [ text ("Error: " ++ err) ]

        Loaded state ->
            viewLoaded state


viewLoaded : PlayerState -> Html Msg
viewLoaded state =
    div [ style "padding" "1rem" ]
        ([ backLink state.path
         , case state.mediaError of
            Just code ->
                viewMediaError state.path code

            Nothing ->
                viewPlayer state
         , h2 [ style "margin-top" "0.75rem" ] [ text state.title ]
         ]
            ++ viewMetadata state
        )


backLink : String -> Html Msg
backLink path =
    a
        [ href (Route.toString (Route.Browse { path = parentPath path, query = "", page = 1 }))
        , style "margin-bottom" "1rem"
        , style "display" "inline-block"
        ]
        [ text "← Back" ]


viewMediaError : String -> MediaErrorCode -> Html Msg
viewMediaError path code =
    div
        [ style "background" "var(--color-surface)"
        , style "padding" "2rem"
        , style "max-width" "960px"
        , style "text-align" "center"
        ]
        [ p [ style "color" "var(--color-error)" ]
            [ text (mediaErrorMessage code) ]
        , a
            [ href (Api.videoUrl path)
            , attribute "download" ""
            ]
            [ text "Download to play in VLC or another media player" ]
        ]


viewPlayer : PlayerState -> Html Msg
viewPlayer state =
    let
        ( extraAttrs, sources ) =
            videoSources state
    in
    div []
        [ bufferBar state.bufferFraction
        , video (videoAttrs ++ extraAttrs) sources
        ]


videoAttrs : List (Attribute Msg)
videoAttrs =
    [ controls True
    , on "canplay" (D.succeed VideoCanPlay)
    , style "width" "100%"
    , style "max-width" "960px"
    , style "display" "block"
    ]


{-| Build the extra attributes and child source elements for the video
tag.  Single-source videos attach the error decoder directly; dual-source
videos use source elements whose type\_ hints and ordering let the
browser pick the best format.
-}
videoSources :
    PlayerState
    -> ( List (Attribute Msg), List (Html Msg) )
videoSources state =
    case state.compatPath of
        Nothing ->
            -- Single source: attach error decoder directly to the video
            -- element where target.error.code is available.
            ( [ src (Api.videoUrl state.path)
              , on "error" (D.map MediaError mediaErrorDecoder)
              ]
            , []
            )

        Just cp ->
            dualSources state.canWebm state.path cp


{-| Build two source elements for a video with a compat MP4 copy.
Error fires on the last source only if both fail; source elements have
no .error.code so we use a fixed code.
-}
dualSources :
    Bool
    -> String
    -> String
    -> ( List (Attribute Msg), List (Html Msg) )
dualSources canWebm path compatPath =
    let
        errorAttr =
            on "error" (D.succeed (MediaError ErrSrcNotSupported))
    in
    case mimeTypeFromPath path of
        Just mime ->
            -- Web-standard format: type_ hints let the browser skip
            -- instantly; canWebm refines source order for older Safari.
            let
                ( first, second ) =
                    if canWebm then
                        ( ( Api.videoUrl path, mime )
                        , ( Api.videoUrl compatPath, "video/mp4" )
                        )

                    else
                        ( ( Api.videoUrl compatPath, "video/mp4" )
                        , ( Api.videoUrl path, mime )
                        )
            in
            ( []
            , [ source [ src (Tuple.first first), type_ (Tuple.second first) ] []
              , source
                    [ src (Tuple.first second)
                    , type_ (Tuple.second second)
                    , errorAttr
                    ]
                    []
              ]
            )

        Nothing ->
            -- Non-standard container (MKV, AVI, MOV): omit type_ so
            -- capable browsers play it directly; others probe the
            -- header bytes and quickly skip to the compat MP4.
            ( []
            , [ source [ src (Api.videoUrl path) ] []
              , source
                    [ src (Api.videoUrl compatPath)
                    , type_ "video/mp4"
                    , errorAttr
                    ]
                    []
              ]
            )


bufferBar : Float -> Html msg
bufferBar fraction =
    if fraction < 1 then
        div
            [ style "width" "100%"
            , style "max-width" "960px"
            , style "height" "4px"
            , style "background" "var(--color-surface)"
            , style "margin-bottom" "0.25rem"
            ]
            [ div
                [ style "height" "100%"
                , style "width" (String.fromFloat (fraction * 100) ++ "%")
                , style "background" "var(--color-link)"
                , style "transition" "width 0.3s ease"
                ]
                []
            ]

    else
        text ""


viewMetadata : PlayerState -> List (Html Msg)
viewMetadata state =
    List.filterMap identity
        [ state.uploadDate
            |> Maybe.map (\d -> p [] [ text ("Uploaded: " ++ formatDate d) ])
        , state.durationSecs
            |> Maybe.map (\s -> p [] [ text ("Duration: " ++ formatDuration s) ])
        , state.viewCount
            |> Maybe.map (\n -> p [] [ text ("Views: " ++ formatViewCount n) ])
        , state.channel
            |> Maybe.map (viewChannel state.channelUrl)
        , state.webpageUrl
            |> Maybe.map viewYoutubeLink
        , state.description
            |> Maybe.map viewDescription
        ]


viewChannel : Maybe String -> String -> Html msg
viewChannel channelUrl ch =
    p []
        [ text "Channel: "
        , case channelUrl of
            Just url ->
                a [ href url, target "_blank", attribute "rel" "noopener noreferrer" ]
                    [ text ch ]

            Nothing ->
                text ch
        ]


viewYoutubeLink : String -> Html msg
viewYoutubeLink url =
    p []
        [ a [ href url, target "_blank", attribute "rel" "noopener noreferrer" ]
            [ text "Watch on YouTube" ]
        ]


viewDescription : String -> Html msg
viewDescription desc =
    div
        [ style "margin-top" "1rem"
        , style "max-width" "960px"
        , style "max-height" "14rem"
        , style "overflow-y" "auto"
        , style "font-size" "0.9rem"
        , style "line-height" "1.6"
        , style "white-space" "pre-wrap"
        , style "word-break" "break-word"
        ]
        (renderDescription desc)



-- HELPERS


parentPath : String -> String
parentPath path =
    path
        |> String.split "/"
        |> List.reverse
        |> List.drop 1
        |> List.reverse
        |> String.join "/"


loadingPath : Model -> String
loadingPath model =
    case model of
        Loading _ p ->
            p

        _ ->
            ""


loadingCanWebm : Model -> Bool
loadingCanWebm model =
    case model of
        Loading cw _ ->
            cw

        _ ->
            True


{-| Map a file path's extension to a web-standard MIME type.  Returns
Nothing for non-standard containers (MKV, AVI, MOV) so the caller can
omit the type\_ attribute and let the browser probe the header bytes.
-}
mimeTypeFromPath : String -> Maybe String
mimeTypeFromPath path =
    let
        ext =
            path
                |> String.split "."
                |> List.reverse
                |> List.head
                |> Maybe.map String.toLower
                |> Maybe.withDefault ""
    in
    case ext of
        "webm" ->
            Just "video/webm"

        "mp4" ->
            Just "video/mp4"

        _ ->
            Nothing


{-| Strip trailing punctuation characters that are unlikely to be part of a
URL even though they are technically valid. If they were intentional they
would normally be percent-encoded.
-}
stripTrailingPunct : String -> ( String, String )
stripTrailingPunct s =
    let
        punctChars =
            [ '.', ',', ';', ')', ']', '!', '"', '\'', '>', '?' ]

        dropRight str =
            case String.uncons (String.reverse str) of
                Just ( c, rest ) ->
                    if List.member c punctChars then
                        dropRight (String.reverse rest)

                    else
                        str

                Nothing ->
                    str

        url =
            dropRight s
    in
    ( url, String.dropLeft (String.length url) s )


{-| Render a description string, turning http/https tokens into clickable
links. Newlines are preserved by the parent's white-space: pre-wrap style.
-}
renderDescription : String -> List (Html msg)
renderDescription desc =
    let
        isUrl w =
            String.startsWith "http://" w || String.startsWith "https://" w

        renderWord w =
            if isUrl w then
                let
                    ( url, trailing ) =
                        stripTrailingPunct w
                in
                span []
                    [ a
                        [ href url
                        , target "_blank"
                        , attribute "rel" "noopener noreferrer"
                        ]
                        [ text url ]
                    , text trailing
                    ]

            else
                text w

        renderLine line =
            if String.isEmpty line then
                [ text "\n" ]

            else
                (line
                    |> String.words
                    |> List.map renderWord
                    |> List.intersperse (text " ")
                )
                    ++ [ text "\n" ]
    in
    desc
        |> String.lines
        |> List.concatMap renderLine


{-| Format a yt-dlp upload\_date string (YYYYMMDD) as YYYY-MM-DD. -}
formatDate : String -> String
formatDate date =
    if String.length date == 8 then
        String.slice 0 4 date
            ++ "-"
            ++ String.slice 4 6 date
            ++ "-"
            ++ String.slice 6 8 date

    else
        date


formatViewCount : Int -> String
formatViewCount n =
    if n >= 1000000 then
        String.fromFloat (toFloat (n // 100000) / 10) ++ "M"

    else if n >= 1000 then
        String.fromFloat (toFloat (n // 100) / 10) ++ "K"

    else
        String.fromInt n


formatDuration : Float -> String
formatDuration secs =
    let
        total =
            round secs

        hours =
            total // 3600

        minutes =
            (total - hours * 3600) // 60

        seconds =
            total - hours * 3600 - minutes * 60

        pad n =
            if n < 10 then
                "0" ++ String.fromInt n

            else
                String.fromInt n
    in
    if hours > 0 then
        String.fromInt hours ++ ":" ++ pad minutes ++ ":" ++ pad seconds

    else
        String.fromInt minutes ++ ":" ++ pad seconds


mediaErrorDecoder : D.Decoder MediaErrorCode
mediaErrorDecoder =
    D.at [ "target", "error", "code" ] D.int
        |> D.map
            (\code ->
                case code of
                    1 ->
                        ErrAborted

                    2 ->
                        ErrNetwork

                    3 ->
                        ErrDecode

                    4 ->
                        ErrSrcNotSupported

                    _ ->
                        ErrUnknown code
            )


mediaErrorMessage : MediaErrorCode -> String
mediaErrorMessage code =
    case code of
        ErrAborted ->
            "Playback was aborted."

        ErrNetwork ->
            "A network error prevented the video from loading."

        ErrDecode ->
            "The video could not be decoded (codec error)."

        ErrSrcNotSupported ->
            "This video format is not supported by your browser (AV1/WebM)."

        ErrUnknown n ->
            "Playback failed (error code " ++ String.fromInt n ++ ")."
